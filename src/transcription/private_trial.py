"""Offline-provisioned, privately distributed trial credential.

No credential-server requests, no personal .env edits, and no key in settings IPC.
This is a distributable long-term credential and is intentionally NOT presented
as secure against extraction by a recipient.
"""
import asyncio
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
import json
import threading
import time
import uuid

import aiohttp

from ..diagnostics import DiagnosticsStore
from ..runtime_paths import DATA_DIR, RESOURCE_ROOT
from .promotion import Credential, MONTH_TIMEZONE
from .private_trial_payload import FILENAME, decode
from .speech_models import DEFAULT_MODELS

MODEL = DEFAULT_MODELS["promotion"]


class PrivateTrialBroker:
    private_trial = True

    def __init__(self, *, directory=DATA_DIR, resource_root=RESOURCE_ROOT):
        self.resource_root = resource_root
        self.state = DiagnosticsStore(directory / "promotion.json", durable=False)
        self._loaded = None
        self._lock = threading.RLock()
        self._pool = ThreadPoolExecutor(max_workers=1, thread_name_prefix="private-trial-check")
        self._check = None
        self._request_id = ""

    def get(self, timeout=4):
        with self._lock:
            if self._loaded is None:
                try:
                    self._loaded = decode((self.resource_root / FILENAME).read_bytes())
                except (OSError, ValueError):
                    raise RuntimeError("内测凭证无法读取，请联系分发者获取新的内测包。") from None
            return Credential(self._loaded.key, time.time() + 3600, self._loaded.host,
                              3600, datetime.now(MONTH_TIMEZONE).strftime("%Y-%m"))

    def prefetch(self):
        # No network acquisition on the shortcut path.
        return None

    def invalidate(self):
        with self._lock:
            self._loaded = None

    def report(self, session_id, seconds, started_at):
        # Provider-side billing only. Do not pretend this offline build enforces
        # the anonymous service's per-device monthly allowance.
        pass

    async def _probe(self):
        credential = self.get()
        timeout = aiohttp.ClientTimeout(total=10, sock_connect=5)
        async with aiohttp.ClientSession(timeout=timeout) as session:
            async with session.ws_connect(
                f"wss://{credential.api_host}/api-ws/v1/inference",
                headers={"Authorization": "Bearer " + credential.token},
            ) as socket:
                task = uuid.uuid4().hex
                await socket.send_json({
                    "header": {"action": "run-task", "task_id": task, "streaming": "duplex"},
                    "payload": {"task_group": "audio", "task": "asr", "function": "recognition",
                                "model": MODEL, "parameters": {"format": "pcm", "sample_rate": 16000},
                                "input": {}},
                })
                for _ in range(8):
                    message = await socket.receive(timeout=8)
                    if message.type != aiohttp.WSMsgType.TEXT:
                        break
                    event = json.loads(message.data).get("header", {}).get("event")
                    if event == "task-started":
                        # Some provider ACL failures arrive after task-started.
                        # Finish a small silent request before declaring success.
                        await socket.send_bytes(bytes(3200))
                        await socket.send_json({
                            "header": {"action": "finish-task", "task_id": task, "streaming": "duplex"},
                            "payload": {"input": {}},
                        })
                    if event == "task-finished":
                        return
                    if event == "task-failed":
                        break
                raise RuntimeError("Trial connection unavailable")

    def request_check(self, request_id):
        def run():
            try:
                asyncio.run(asyncio.wait_for(self._probe(), timeout=10))
                status, enabled = "内置服务已连接，正在保存并应用…", True
            except Exception:
                # Never expose provider errors, URLs with headers, or key values.
                status, enabled = "内置服务连接失败，请检查网络或切换个人 API。", False
            with self._lock:
                self.state.update(status=status, enabled=enabled, model=MODEL,
                                  remaining_seconds=3600, request_id=self._request_id, private_trial=True)
        with self._lock:
            self._request_id = request_id
            if self._check is None or self._check.done():
                self._check = self._pool.submit(run)
            return self._check
