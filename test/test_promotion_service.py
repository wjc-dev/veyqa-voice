import json
import tempfile
import time
import unittest
import uuid
from pathlib import Path
from server.promotion import Ledger, ServiceError, RateLimiter, month_at


class PromotionLedgerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.ledger = Ledger(Path(self.temp.name) / "usage.sqlite3")
        self.device = uuid.uuid4().hex
        self.secret = "a" * 48
        self.now = time.time()
        self.ledger.register(self.device, self.secret, self.now)

    def report(self, seconds, session=None):
        return self.ledger.report(self.device, {"session_id": session or uuid.uuid4().hex,
            "seconds": seconds, "started_at": self.now}, self.now)

    def test_device_authentication_and_idempotent_registration(self):
        self.ledger.register(self.device, self.secret, self.now)
        self.assertEqual(self.ledger.authenticate("Bearer " + self.device + "." + self.secret), self.device)
        with self.assertRaises(ServiceError):
            self.ledger.register(self.device, "b" * 48, self.now)
        with self.assertRaises(ServiceError):
            self.ledger.authenticate("Bearer " + self.device + ".wrong")

    def test_exact_allowance_boundary_and_idempotency(self):
        session = uuid.uuid4().hex
        self.report(3000, session)
        self.report(3000, session)
        self.assertEqual(self.ledger.status(self.device, self.now)["remaining_seconds"], 3000)
        self.assertEqual(self.report(3000)["remaining_seconds"], 0)
        with self.assertRaises(ServiceError):
            self.report(1, session)

    def test_invalid_usage_rejected(self):
        for value in (-1, True, 3601, float("nan"), "100"):
            with self.subTest(value=value), self.assertRaises(ServiceError):
                self.report(value)

    def test_cross_device_session_cannot_be_overwritten(self):
        session = uuid.uuid4().hex
        self.report(3, session)
        other = uuid.uuid4().hex
        self.ledger.register(other, "b" * 48, self.now)
        with self.assertRaises(ServiceError):
            self.ledger.report(other, {"session_id": session, "seconds": 3,
                "started_at": self.now}, self.now)

    def test_month_uses_shanghai_and_old_usage_does_not_reduce_new_month(self):
        self.assertEqual(month_at(1793462400), "2026-11")
        self.report(10)
        later = self.now + 32 * 86400
        self.assertEqual(self.ledger.status(self.device, later)["remaining_seconds"], 6000)

    def test_rate_limit_window_resets(self):
        limiter = RateLimiter()
        self.assertTrue(limiter.allow("key", 1, 0))
        self.assertFalse(limiter.allow("key", 1, 1))
        self.assertTrue(limiter.allow("key", 1, 3601))
