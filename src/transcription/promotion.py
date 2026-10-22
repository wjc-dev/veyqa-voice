"""Prefetched temporary credentials; audio never passes through this service."""
from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeout
import ctypes
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
import json
import math
import os
import re
import secrets
import threading
import time
import uuid
from urllib.error import HTTPError
from urllib.request import HTTPRedirectHandler, Request, build_opener

from ..diagnostics import DiagnosticsStore
from ..runtime_paths import DATA_DIR
from ..release_features import builtin_speech_enabled

SERVICE_URL = "https://rtcut.org/veyqa-api"
MODEL = "qwen-audio-3.0-asr-flash-streaming"
MONTH_TIMEZONE = timezone(timedelta(hours=8))
ERRORS = {
    "monthly_allowance_used": "本月免费 100 分钟已用完，可在设置中使用个人 API。",
    "promotion_unavailable": "内置服务暂不可用，可稍后重试或使用个人 API。",
    "try_later": "连接请求较多，请稍后再试。",
}


class NoCredentialRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def uses_builtin():
    if not builtin_speech_enabled():
        return False
    source = os.getenv("SPEECH_CREDENTIAL_SOURCE", "").strip()
    if source:
        return source == "builtin"
    return not any(os.getenv(key, "").strip() for key in (
        "QWEN_API_KEY", "DASHSCOPE_API_KEY", "DOUBAO_ACCESS_KEY", "CUSTOM_ASR_MODEL"))


def keychain_identity():
    """Keep an anonymous device credential out of argv, .env and diagnostics."""
    import objc
    from Foundation import NSData, NSDictionary
    security = ctypes.CDLL("/System/Library/Frameworks/Security.framework/Security")
    foundation = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
    foundation.CFRelease.argtypes = [ctypes.c_void_p]
    def constant(name):
        return objc.objc_object(c_void_p=ctypes.c_void_p.in_dll(security, name).value)
    def pointer(value):
        return ctypes.c_void_p(objc.pyobjc_id(value))
    query = {constant("kSecClass"): constant("kSecClassGenericPassword"),
             constant("kSecAttrService"): "com.wjcdev.veyqa.promotion.device",
             constant("kSecAttrAccount"): "anonymous-device"}
    read = NSDictionary.dictionaryWithDictionary_({**query, constant("kSecReturnData"): True,
        constant("kSecMatchLimit"): constant("kSecMatchLimitOne")})
    security.SecItemCopyMatching.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)]
    security.SecItemCopyMatching.restype = ctypes.c_int32
    output = ctypes.c_void_p()
    status = security.SecItemCopyMatching(pointer(read), ctypes.byref(output))
    if status == 0:
        try:
            identity = json.loads(bytes(objc.objc_object(c_void_p=output.value)))
            if (re.fullmatch(r"[a-f0-9]{32}", identity.get("device_id", ""))
                    and re.fullmatch(r"[A-Za-z0-9_-]{43,128}", identity.get("secret", ""))):
                return identity
        finally:
            foundation.CFRelease(output)
        raise RuntimeError("设备凭证无法读取，请稍后重试。")
    if status != -25300:  # errSecItemNotFound only; do not overwrite inaccessible items.
        raise RuntimeError("无法访问设备钥匙串，请解锁钥匙串后重试。")
    identity = {"device_id": uuid.uuid4().hex, "secret": secrets.token_urlsafe(32)}
    data = json.dumps(identity).encode()
    attributes = NSDictionary.dictionaryWithDictionary_({**query,
        constant("kSecValueData"): NSData.dataWithBytes_length_(data, len(data))})
    security.SecItemAdd.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    security.SecItemAdd.restype = ctypes.c_int32
    if security.SecItemAdd(pointer(attributes), None) != 0:
        raise RuntimeError("无法保存设备凭证，请检查钥匙串访问权限。")
    return identity


@dataclass(frozen=True, repr=False)
class Credential:
    token: str
    expires_at: float
    api_host: str
    remaining_seconds: int
    month: str


class CredentialBroker:
    def __init__(self, *, identity_loader=keychain_identity, transport=None, directory=DATA_DIR, clock=time.time):
        self.identity_loader = identity_loader
        self.transport = transport or self._http
        self.clock = clock
        self.directory = directory
        self.identity = None
        self.registered = False
        self._credential = None
        self._future = None
        self._lock = threading.RLock()
        self._pool = ThreadPoolExecutor(max_workers=1, thread_name_prefix="speech-credentials")
        self.state = DiagnosticsStore(directory / "promotion.json", durable=False)
        self.outbox = directory / "promotion-usage.json"
        self._retry_after = 0

    def _month(self):
        return datetime.fromtimestamp(self.clock(), MONTH_TIMEZONE).strftime("%Y-%m")

    def _cache_valid(self):
        return (self._credential is not None
                and self._credential.expires_at > self.clock() + 20
                and self._credential.month == self._month())

    def _pending_seconds(self, pending, month):
        return sum(record["seconds"] for record in pending
                   if datetime.fromtimestamp(record["started_at"], MONTH_TIMEZONE).strftime("%Y-%m") == month)

    def _http(self, path, body):
        headers = {"Content-Type": "application/json", "User-Agent": "Veyqa-Voice"}
        if self.identity and path != "/v1/devices":
            headers["Authorization"] = "Bearer " + self.identity["device_id"] + "." + self.identity["secret"]
        request = Request(SERVICE_URL + path, data=json.dumps(body).encode(), headers=headers)
        try:
            with build_opener(NoCredentialRedirect()).open(request, timeout=3) as response:
                return json.loads(response.read(16384))
        except HTTPError as exc:
            try:
                code = json.loads(exc.read(4096)).get("error", "")
            except (ValueError, OSError):
                code = ""
            if exc.code == 401:
                self.registered = False
            raise RuntimeError(ERRORS.get(code, "内置服务连接失败，请稍后重试。")) from None
        except (OSError, ValueError):
            raise RuntimeError("无法连接内置服务，请检查网络或使用个人 API。") from None

    def _publish(self, data, status="已就绪"):
        # Deliberately whitelist fields: provider token and device secret must
        # never cross the main-process/settings IPC boundary.
        self.state.update(status=status, model=MODEL,
            month=str(data.get("month", "")),
            remaining_seconds=int(data.get("remaining_seconds", 6000)),
            enabled=data.get("enabled") is True and data.get("budget_available", True) is True)

    def _fetch(self):
        try:
            if self.identity is None:
                self.identity = self.identity_loader()
            if not self.registered:
                self.transport("/v1/devices", self.identity)
                self.registered = True
            # Never renew the allowance while earlier consumption is unreported.
            self._flush(strict=True, refresh=False)
            body = self.transport("/v1/credentials", {})
            token, expires, host = body.get("token"), body.get("expires_at"), body.get("api_host")
            if (not isinstance(token, str) or not token.startswith("st-")
                    or type(expires) not in (int, float) or not self.clock()+15 < expires < self.clock()+1830
                    or not isinstance(host, str)
                    or not re.fullmatch(r"[a-z0-9-]+\.cn-beijing\.maas\.aliyuncs\.com", host)
                    or body.get("model") != MODEL
                    or body.get("month") != self._month()
                    or body.get("enabled") is not True
                    or type(body.get("remaining_seconds")) is not int
                    or not 0 < body["remaining_seconds"] <= 6000):
                raise RuntimeError("内置服务返回了无效配置，请稍后重试。")
            with self._lock:
                remaining = max(0, body["remaining_seconds"] -
                                self._pending_seconds(self._read_outbox(), body["month"]))
                credential = Credential(token, expires, host, remaining, body["month"])
                self._credential = credential
                self._retry_after = 0
            self._publish({**body, "remaining_seconds": remaining})
            return credential
        except Exception as exc:
            with self._lock:
                self._retry_after = self.clock() + 10
            message = str(exc) if isinstance(exc, RuntimeError) else "内置服务暂不可用，请稍后重试。"
            self.state.update(status=message, model=MODEL, enabled=False)
            raise RuntimeError(message) from None

    def prefetch(self):
        with self._lock:
            if self._cache_valid():
                return None
            if self._future and not self._future.done():
                return self._future
            if self.clock() < self._retry_after:
                return self._future
            self._future = self._pool.submit(self._fetch)
            return self._future

    def get(self, timeout=4):
        with self._lock:
            if self._cache_valid():
                if self._credential.remaining_seconds <= 0:
                    raise RuntimeError(ERRORS["monthly_allowance_used"])
                return self._credential
        future = self.prefetch()
        try:
            # Another thread can complete prefetch between the two cache reads.
            if future is not None:
                future.result(timeout=timeout)
            with self._lock:
                if self._cache_valid():
                    if self._credential.remaining_seconds <= 0:
                        raise RuntimeError(ERRORS["monthly_allowance_used"])
                    return self._credential
            raise RuntimeError(ERRORS["promotion_unavailable"])
        except FutureTimeout:
            raise RuntimeError("连接内置服务超时，请稍后重试。") from None

    def invalidate(self):
        with self._lock:
            self._credential = None
            self._retry_after = 0

    def request_check(self, request_id):
        future = self.prefetch()
        def finished(value=None):
            try:
                credential = value.result() if value else self.get()
                self.state.update(status="已就绪", model=MODEL, enabled=True,
                    remaining_seconds=credential.remaining_seconds)
            except Exception:
                pass
            self.state.update(request_id=request_id)
        if future is None:
            finished()
        else:
            future.add_done_callback(finished)

    def _read_outbox(self):
        try:
            data = json.loads(self.outbox.read_text())
            if not isinstance(data, list):
                raise ValueError()
            return data
        except FileNotFoundError:
            return []
        except (OSError, ValueError):
            raise RuntimeError("内置服务用量记录无法读取，请使用个人 API 或联系支持。") from None

    def report(self, session_id, seconds, started_at):
        if seconds <= 0:
            self.prefetch()
            return
        with self._lock:
            pending = self._read_outbox()
            record = {"session_id": session_id, "seconds": min(3600, math.ceil(seconds)),
                      "started_at": started_at}
            if any(item["session_id"] == session_id for item in pending):
                return
            pending.append(record)
            DiagnosticsStore(self.outbox)._save(pending)
            # Reflect consumption immediately even while report is offline.
            if self._credential:
                old = self._credential
                consumed = self._pending_seconds([record], old.month)
                self._credential = Credential(old.token, old.expires_at, old.api_host,
                    max(0, old.remaining_seconds-consumed), old.month)
        self._pool.submit(self._flush)

    def _flush(self, *, strict=False, refresh=True):
        if not self.identity:
            return
        try:
            with self._lock:
                pending = self._read_outbox()
            # Drain the snapshot in bounded requests; concurrent reports remain
            # in the outbox and are subtracted from each server acknowledgement.
            for offset in range(0, len(pending), 32):
                batch = pending[offset:offset + 32]
                status = self.transport("/v1/usage", {"records": batch})
                if (type(status.get("remaining_seconds")) is not int
                        or not 0 <= status["remaining_seconds"] <= 6000
                        or status.get("month") != self._month()):
                    raise RuntimeError("内置服务用量同步失败，请稍后重试。")
                sent = {r["session_id"] for r in batch}
                with self._lock:
                    remaining_records = [r for r in self._read_outbox() if r["session_id"] not in sent]
                    DiagnosticsStore(self.outbox)._save(remaining_records)
                    remaining = max(0, status["remaining_seconds"] -
                                    self._pending_seconds(remaining_records, status["month"]))
                    status = {**status, "remaining_seconds": remaining}
                    if self._credential:
                        old = self._credential
                        if status.get("enabled") is not True or status.get("budget_available", True) is not True:
                            self._credential = None
                        else:
                            self._credential = Credential(old.token, old.expires_at, old.api_host,
                                remaining, old.month)
                self._publish(status)
            if refresh:
                self.prefetch()
        except Exception:
            if strict:
                raise RuntimeError("内置服务用量同步失败，请稍后重试或使用个人 API。") from None
            # Keep pending records for the next report/credential renewal.


_broker = None
_broker_lock = threading.Lock()

def get_broker():
    global _broker
    with _broker_lock:
        if _broker is None:
            from ..release_features import private_trial_enabled
            if private_trial_enabled():
                from .private_trial import PrivateTrialBroker
                _broker = PrivateTrialBroker()
            else:
                _broker = CredentialBroker()
        return _broker
