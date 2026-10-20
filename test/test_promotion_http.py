"""Loopback end-to-end tests; no real provider key or paid recognition."""
import json
import tempfile
import threading
import time
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from server.promotion import BoundedHTTPServer, Ledger, handler_for
from src.transcription.promotion import CredentialBroker, MODEL


class PromotionHTTPTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        for name in ("enabled", "budget", "key"):
            (self.directory / name).write_text("test-only")
        self.ledger = Ledger(self.directory / "usage.sqlite")
        handler = handler_for(self.ledger, key_file=self.directory / "key",
                              budget_marker=self.directory / "budget", enabled_file=self.directory / "enabled",
                              api_host="ws-test.cn-beijing.maas.aliyuncs.com")
        self.server = BoundedHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        def stop():
            self.server.shutdown()
            self.server.server_close()
            thread.join(timeout=2)
        self.addCleanup(stop)
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        patcher = patch("src.transcription.promotion.SERVICE_URL", self.url)
        patcher.start()
        self.addCleanup(patcher.stop)
        minter = patch("server.promotion.mint_token", side_effect=lambda key: {
            "token": "st-test-only", "expires_at": time.time() + 300, "model": MODEL})
        self.mint = minter.start()
        self.addCleanup(minter.stop)
        self.broker = CredentialBroker(directory=self.directory,
            identity_loader=lambda: {"device_id": uuid.uuid4().hex, "secret": "a" * 48})
        self.addCleanup(self.broker._pool.shutdown)

    def test_registration_prefetch_report_and_service_disable(self):
        self.assertEqual(self.broker.get().remaining_seconds, 6000)
        self.assertEqual(self.mint.call_count, 1)
        self.broker.report(uuid.uuid4().hex, 3.2, time.time())
        self.broker._pool.submit(lambda: None).result(timeout=5)
        self.assertEqual(self.broker.get().remaining_seconds, 5996)
        self.assertEqual(self.ledger.status(self.broker.identity["device_id"], time.time())["remaining_seconds"], 5996)
        (self.directory / "enabled").unlink()
        self.broker.invalidate()
        with self.assertRaisesRegex(RuntimeError, "暂不可用"):
            self.broker.get()
        self.assertEqual(self.mint.call_count, 1)

    def test_missing_budget_gate_never_mints_token(self):
        (self.directory / "budget").unlink()
        with self.assertRaisesRegex(RuntimeError, "暂不可用"):
            self.broker.get()
        self.assertEqual(self.mint.call_count, 0)

    def test_unauthenticated_request_never_mints_token(self):
        request = Request(self.url + "/v1/credentials", data=b"{}",
                          headers={"Content-Type": "application/json"})
        with self.assertRaises(HTTPError) as result:
            urlopen(request, timeout=2)
        self.assertEqual(result.exception.code, 401)
        self.assertEqual(self.mint.call_count, 0)
