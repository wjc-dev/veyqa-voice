import json
import os
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from src.transcription.promotion import CredentialBroker, MODEL, uses_builtin


class CredentialCacheTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.now = 1000
        self.calls = []
        def transport(path, body):
            self.calls.append(path)
            if path == "/v1/devices":
                return {"registered": True}
            return {"token": "st-test-only", "expires_at": self.now + 300,
                    "api_host": "ws-example.cn-beijing.maas.aliyuncs.com",
                    "model": MODEL, "month": "2026-10", "remaining_seconds": 6000, "enabled": True}
        self.broker = CredentialBroker(identity_loader=lambda: {"device_id": "a"*32, "secret": "b"*43},
            transport=transport, directory=Path(self.temp.name), clock=lambda: self.now)
        self.addCleanup(self.broker._pool.shutdown)

    def test_prefetch_eliminates_hot_path_network_and_merges_requests(self):
        self.broker.prefetch().result(timeout=2)
        self.calls.clear()
        first = self.broker.get()
        self.assertIs(first, self.broker.get())
        self.assertIsNone(self.broker.prefetch())
        self.assertEqual(self.calls, [])

    def test_expired_credential_refreshes_without_re_registering(self):
        self.broker.get()
        self.now += 290
        self.broker.get()
        self.assertEqual(self.calls.count("/v1/devices"), 1)
        self.assertEqual(self.calls.count("/v1/credentials"), 2)

    def test_provider_secrets_never_written_to_settings_state(self):
        self.broker.get()
        text = (Path(self.temp.name)/"promotion.json").read_text()
        self.assertNotIn("st-test-only", text)
        self.assertNotIn("b"*43, text)
        self.assertEqual(json.loads(text)["model"], MODEL)

    def test_failed_prefetch_is_bounded_and_retry_uses_backoff(self):
        def fail(path, body): raise RuntimeError("temporary outage")
        self.broker.transport = fail
        for _ in range(2):
            with self.assertRaisesRegex(RuntimeError, "temporary outage"):
                self.broker.get()
        self.now += 11
        self.assertTrue(self.broker.prefetch() is not None)

    def test_untrusted_upstream_and_models_are_rejected(self):
        original = self.broker.transport
        def malicious(path, body):
            data = original(path, body)
            if path == "/v1/credentials": data["api_host"] = "attacker.example"
            return data
        self.broker.transport = malicious
        with self.assertRaisesRegex(RuntimeError, "无效配置"):
            self.broker.get()

    def test_release_routes_new_and_existing_users_to_personal_credentials(self):
        with patch.dict(os.environ, {}, clear=True):
            self.assertFalse(uses_builtin())
            os.environ["QWEN_API_KEY"] = "existing-key"
            self.assertFalse(uses_builtin())
            os.environ["SPEECH_CREDENTIAL_SOURCE"] = "builtin"
            self.assertFalse(uses_builtin())
            os.environ["SPEECH_CREDENTIAL_SOURCE"] = "personal"
            self.assertFalse(uses_builtin())
