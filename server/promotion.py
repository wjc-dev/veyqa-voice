"""Anonymous promotional credential broker. Never handles audio or transcripts.

The device allowance is cooperative, not adversarial metering. A verified
provider-side budget and model-restricted parent key are required before enable.
"""
from __future__ import annotations

import hashlib
import hmac
import json
import math
import os
from pathlib import Path
import re
import secrets
import sqlite3
import threading
import time
from datetime import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from zoneinfo import ZoneInfo

MODEL = "qwen-audio-3.0-asr-flash-streaming"
MONTHLY_SECONDS = 6000
MONTHLY_MICROYUAN = 100_000_000
MICROYUAN_PER_SECOND = 330
TOKEN_TTL = 300
IDENTIFIER = re.compile(r"[a-f0-9-]{32,36}\Z")


class ServiceError(Exception):
    def __init__(self, status, code):
        self.status, self.code = status, code


def month_at(timestamp):
    return datetime.fromtimestamp(timestamp, ZoneInfo("Asia/Shanghai")).strftime("%Y-%m")


class Ledger:
    def __init__(self, path):
        self.path = str(path)
        with self.connect() as db:
            db.executescript("""
                CREATE TABLE IF NOT EXISTS devices (
                    id TEXT PRIMARY KEY, secret_hash TEXT NOT NULL, created REAL NOT NULL);
                CREATE TABLE IF NOT EXISTS usage (
                    id TEXT PRIMARY KEY, device TEXT NOT NULL, month TEXT NOT NULL,
                    seconds INTEGER NOT NULL, created REAL NOT NULL);
                CREATE INDEX IF NOT EXISTS usage_month ON usage(month, device);
            """)

    def connect(self):
        return sqlite3.connect(self.path, timeout=3)

    def register(self, device, secret, now):
        if (not isinstance(device, str) or not IDENTIFIER.fullmatch(device)
                or not isinstance(secret, str) or not re.fullmatch(r"[A-Za-z0-9_-]{43,128}", secret)):
            raise ServiceError(400, "invalid_device")
        digest = hashlib.sha256(secret.encode()).hexdigest()
        with self.connect() as db:
            db.execute("INSERT OR IGNORE INTO devices VALUES (?,?,?)", (device, digest, now))
            stored = db.execute("SELECT secret_hash FROM devices WHERE id=?", (device,)).fetchone()
        if not stored or not hmac.compare_digest(stored[0], digest):
            raise ServiceError(401, "device_authentication_failed")

    def authenticate(self, authorization):
        value = authorization.removeprefix("Bearer ").split(".", 1)
        if len(value) != 2 or not IDENTIFIER.fullmatch(value[0]) or len(value[1]) > 128:
            raise ServiceError(401, "device_authentication_failed")
        with self.connect() as db:
            row = db.execute("SELECT secret_hash FROM devices WHERE id=?", (value[0],)).fetchone()
        if not row or not hmac.compare_digest(row[0], hashlib.sha256(value[1].encode()).hexdigest()):
            raise ServiceError(401, "device_authentication_failed")
        return value[0]

    def status(self, device, now):
        month = month_at(now)
        with self.connect() as db:
            own, total = db.execute("""SELECT
                COALESCE(SUM(CASE WHEN device=? THEN seconds ELSE 0 END),0),
                COALESCE(SUM(seconds),0) FROM usage WHERE month=?""", (device, month)).fetchone()
        return {"month": month, "model": MODEL, "limit_seconds": MONTHLY_SECONDS,
                "remaining_seconds": max(0, MONTHLY_SECONDS - own),
                "budget_available": total * MICROYUAN_PER_SECOND < MONTHLY_MICROYUAN}

    def report(self, device, body, now):
        session, seconds, started = body.get("session_id"), body.get("seconds"), body.get("started_at")
        if (not isinstance(session, str) or not IDENTIFIER.fullmatch(session)
                or type(seconds) is not int or not 0 <= seconds <= 3600
                or type(started) not in (int, float) or not math.isfinite(started)
                or not 0 <= now - started <= 7 * 86400):
            raise ServiceError(400, "invalid_usage")
        with self.connect() as db:
            # Globally unique session IDs prevent cross-device overwrite/replay.
            db.execute("BEGIN IMMEDIATE")
            previous = db.execute("SELECT device,seconds FROM usage WHERE id=?", (session,)).fetchone()
            if previous and previous != (device, seconds):
                raise ServiceError(409, "session_conflict")
            db.execute("INSERT OR IGNORE INTO usage VALUES (?,?,?,?,?)",
                       (session, device, month_at(started), seconds, now))
        return self.status(device, now)


class RateLimiter:
    def __init__(self):
        self.entries, self.lock = {}, threading.Lock()

    def allow(self, key, limit, now):
        window = int(now // 3600)
        with self.lock:
            if len(self.entries) > 10000:
                self.entries = {k: v for k, v in self.entries.items() if v[0] == window}
                if len(self.entries) > 10000:
                    return False
            old_window, count = self.entries.get(key, (window, 0))
            count = count if old_window == window else 0
            if count >= limit:
                return False
            self.entries[key] = (window, count + 1)
            return True


def mint_token(key, ttl=TOKEN_TTL):
    request = Request(f"https://dashscope.aliyuncs.com/api/v1/tokens?expire_in_seconds={ttl}",
                      data=b"", method="POST", headers={"Authorization": "Bearer " + key})
    try:
        with urlopen(request, timeout=5) as response:
            body = json.loads(response.read(16384))
    except (OSError, ValueError, HTTPError):
        raise ServiceError(503, "provider_unavailable") from None
    token, expires = body.get("token"), body.get("expires_at")
    if (not isinstance(token, str) or not token.startswith("st-")
            or type(expires) not in (int, float) or not time.time() < expires <= time.time() + ttl + 30):
        raise ServiceError(503, "provider_unavailable")
    return {"token": token, "expires_at": expires}


def handler_for(ledger, *, key_file, budget_marker, enabled_file, api_host):
    if not re.fullmatch(r"[a-z0-9-]+\.cn-beijing\.maas\.aliyuncs\.com", api_host):
        raise ValueError("A Beijing workspace-specific official API host is required")
    limiter = RateLimiter()

    class Handler(BaseHTTPRequestHandler):
        server_version = "Veyqa"
        def log_message(self, *args):
            pass  # Never log request bodies, headers, device secrets or tokens.

        def setup(self):
            super().setup()
            self.connection.settimeout(10)

        def respond(self, status, body):
            data = json.dumps(body, separators=(",", ":")).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if self.path == "/health":
                self.respond(200, {"service": "veyqa-credentials", "enabled":
                    Path(enabled_file).exists() and Path(budget_marker).exists()})
            else:
                self.respond(404, {"error": "not_found"})

        def do_POST(self):
            try:
                if self.path not in {"/v1/devices", "/v1/status", "/v1/credentials", "/v1/usage"}:
                    raise ServiceError(404, "not_found")
                size = int(self.headers.get("Content-Length", "0"))
                if not 0 < size <= 8192 or self.headers.get("Transfer-Encoding"):
                    raise ServiceError(400, "invalid_request")
                body = json.loads(self.rfile.read(size))
                if not isinstance(body, dict):
                    raise ServiceError(400, "invalid_request")
                now = time.time()
                peer = self.headers.get("X-Real-IP", self.client_address[0])
                if not limiter.allow("ip:" + hashlib.sha256(peer.encode()).hexdigest(), 7200, now):
                    raise ServiceError(429, "try_later")
                if self.path == "/v1/devices":
                    if not limiter.allow("register:" + peer, 30, now):
                        raise ServiceError(429, "try_later")
                    ledger.register(body.get("device_id"), body.get("secret"), now)
                    self.respond(200, {"registered": True})
                    return
                device = ledger.authenticate(self.headers.get("Authorization", ""))
                if self.path == "/v1/usage":
                    records = body.get("records", [body])
                    if not isinstance(records, list) or not 1 <= len(records) <= 32:
                        raise ServiceError(400, "invalid_usage")
                    for record in records:
                        if not isinstance(record, dict):
                            raise ServiceError(400, "invalid_usage")
                        status = ledger.report(device, record, now)
                    status["enabled"] = (Path(enabled_file).exists()
                        and Path(budget_marker).exists() and status["budget_available"])
                    self.respond(200, status)
                    return
                status = ledger.status(device, now)
                enabled = Path(enabled_file).exists() and Path(budget_marker).exists()
                status["enabled"] = enabled and status["budget_available"]
                if self.path == "/v1/status":
                    self.respond(200, status)
                    return
                if not status["enabled"]:
                    raise ServiceError(503, "promotion_unavailable")
                if status["remaining_seconds"] <= 0:
                    raise ServiceError(403, "monthly_allowance_used")
                if not limiter.allow("token:" + device, 60, now):
                    raise ServiceError(429, "try_later")
                token = mint_token(Path(key_file).read_text().strip())
                self.respond(200, {**status, **token, "api_host": api_host})
            except ServiceError as exc:
                self.respond(exc.status, {"error": exc.code})
            except (ValueError, UnicodeError, TimeoutError):
                self.respond(400, {"error": "invalid_request"})
            except Exception:
                self.respond(503, {"error": "service_unavailable"})

    return Handler


class BoundedHTTPServer(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 16
    def __init__(self, *args):
        self.slots = threading.BoundedSemaphore(24)
        super().__init__(*args)

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, address)
        except Exception:
            self.slots.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.slots.release()


if __name__ == "__main__":
    os.umask(0o077)
    directory = Path(os.environ.get("VEYQA_SERVICE_DATA", "/var/lib/veyqa-voice"))
    directory.mkdir(parents=True, exist_ok=True)
    config = Path(os.environ.get("VEYQA_SERVICE_CONFIG", "/etc/veyqa-voice"))
    handler = handler_for(Ledger(directory / "usage.sqlite3"), key_file=config / "bailian-key",
        budget_marker=config / "budget-verified", enabled_file=config / "enabled",
        api_host=os.environ["VEYQA_BAILIAN_HOST"])
    BoundedHTTPServer(("127.0.0.1", 19120), handler).serve_forever(poll_interval=1)
