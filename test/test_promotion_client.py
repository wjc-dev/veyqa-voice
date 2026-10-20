import json
import os
import tempfile
import threading
import time
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch

from src.transcription.promotion import CredentialBroker, MODEL, uses_builtin


class CredentialCacheTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.now = 1792454400
        self.calls = []
        def transport(path, body):
            self.calls.append(path)
            if path == "/v1/devices":
                return {"registered": True}
            return {"token": "st-test-only", "expires_at": self.now + 300,
                    "api_host": "ws-example.cn-beijing.maas.aliyuncs.com",
                    "model": MODEL, "month": self.broker._month(), "remaining_seconds": 6000, "enabled": True}
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

    def test_enabled_release_preserves_existing_keys_and_explicit_choice(self):
        with patch("src.transcription.promotion.builtin_speech_enabled", return_value=True), \
                patch.dict(os.environ, {}, clear=True):
            self.assertTrue(uses_builtin())
            os.environ["QWEN_API_KEY"] = "existing-test-key"
            self.assertFalse(uses_builtin())
            os.environ["SPEECH_CREDENTIAL_SOURCE"] = "builtin"
            self.assertTrue(uses_builtin())
            os.environ["SPEECH_CREDENTIAL_SOURCE"] = "personal"
            self.assertFalse(uses_builtin())

    def test_month_boundary_refreshes_unexpired_token(self):
        self.now = 1793462390  # October 31, 23:59:50 Shanghai.
        self.broker.get()
        self.now += 20
        credential = self.broker.get()
        self.assertEqual(credential.month, "2026-11")
        self.assertEqual(self.calls.count("/v1/credentials"), 2)

    def test_cache_completed_between_get_and_prefetch(self):
        credential = self.broker.get()
        self.broker.invalidate()
        def finish_concurrently():
            self.broker._credential = credential
            return None
        with patch.object(self.broker, "prefetch", side_effect=finish_concurrently):
            self.assertIs(self.broker.get(), credential)

    def seed_pending(self, count):
        records = [{"session_id": uuid.uuid4().hex, "seconds": 1, "started_at": self.now}
                   for _ in range(count)]
        self.broker.outbox.write_text(json.dumps(records))
        return records

    def test_all_pending_batches_flush_before_credential_renewal(self):
        self.seed_pending(130)
        self.broker.get()
        self.assertEqual(self.calls, ["/v1/devices"] + ["/v1/usage"] * 5 + ["/v1/credentials"])
        self.assertEqual(self.broker._read_outbox(), [])

    def test_failed_report_stays_on_disk_and_blocks_token_renewal(self):
        records = self.seed_pending(2)
        original = self.broker.transport
        def fail_usage(path, body):
            if path == "/v1/usage":
                raise OSError("offline")
            return original(path, body)
        self.broker.transport = fail_usage
        with self.assertRaisesRegex(RuntimeError, "用量同步失败"):
            self.broker.get()
        self.assertEqual(self.broker._read_outbox(), records)
        self.assertNotIn("/v1/credentials", self.calls)
        self.broker.transport = original
        self.now += 11
        self.broker.get()
        self.assertEqual(self.broker._read_outbox(), [])

    def test_report_does_not_drop_old_records_or_refresh_exhausted_allowance(self):
        self.broker.get()
        self.seed_pending(130)
        with patch.object(self.broker._pool, "submit"):
            self.broker.report(uuid.uuid4().hex, 3600, self.now)
            self.broker.report(uuid.uuid4().hex, 2400, self.now)
        self.assertEqual(len(self.broker._read_outbox()), 132)
        self.calls.clear()
        with self.assertRaisesRegex(RuntimeError, "100 分钟"):
            self.broker.get()
        self.assertEqual(self.calls, [])

    def test_acknowledgement_deducts_concurrent_unreported_usage(self):
        self.broker.get()
        self.seed_pending(1)
        original = self.broker.transport
        def concurrent(path, body):
            if path == "/v1/usage":
                with patch.object(self.broker._pool, "submit"):
                    self.broker.report(uuid.uuid4().hex, 5, self.now)
                return {"month": self.broker._month(), "remaining_seconds": 5999, "enabled": True}
            return original(path, body)
        self.broker.transport = concurrent
        self.broker._flush(refresh=False)
        self.assertEqual(self.broker.get().remaining_seconds, 5994)
        self.assertEqual(len(self.broker._read_outbox()), 1)

    def test_disabled_service_acknowledgement_invalidates_cached_token(self):
        self.broker.get()
        self.seed_pending(1)
        self.broker.transport = lambda path, body: {
            "month": self.broker._month(), "remaining_seconds": 5999, "enabled": False}
        self.broker._flush(refresh=False)
        self.assertIsNone(self.broker._credential)
        state = json.loads((Path(self.temp.name) / "promotion.json").read_text())
        self.assertFalse(state["enabled"])
