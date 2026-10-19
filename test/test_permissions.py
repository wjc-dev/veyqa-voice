import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

from src.permissions import PermissionMonitor, required_permissions_granted


class PermissionLogicTests(unittest.TestCase):
    def test_granted_permissions_do_not_claim_a_working_listener(self):
        with tempfile.TemporaryDirectory() as directory:
            ready = unittest.mock.Mock(return_value=False)
            monitor = PermissionMonitor(version="test", hotkey_backend="passive",
                                        listener_ready=ready,
                                        status_path=Path(directory) / "permissions.json")
            with patch("src.permissions._microphone_status", return_value="granted"), \
                    patch("src.permissions._accessibility_status", return_value="granted"), \
                    patch("src.permissions._input_monitoring_status", return_value="granted"), \
                    patch.object(monitor, "_atomic_write", wraps=monitor._atomic_write) as write:
                snapshot = monitor.refresh()
                self.assertTrue(snapshot["all_required_granted"])
                self.assertFalse(snapshot["hotkey_ready"])
                ready.return_value = True
                self.assertTrue(monitor.refresh()["hotkey_ready"])
                monitor.refresh()
                self.assertEqual(write.call_count, 2)

    def test_request_all_only_requests_missing_required_permissions(self):
        for backend, input_required in (("registered", False), ("passive", True)):
            with self.subTest(backend=backend), tempfile.TemporaryDirectory() as directory:
                monitor = PermissionMonitor(version="test", hotkey_backend=backend,
                                            status_path=Path(directory) / "permissions.json")
                with (
                    patch.object(monitor, "_request_microphone") as mic,
                    patch.object(monitor, "_request_accessibility") as access,
                    patch.object(monitor, "_request_input_monitoring") as keyboard,
                    patch.object(monitor, "_current_status", side_effect=lambda name: "granted" if name == "microphone" else "missing"),
                ):
                    monitor._request("all", open_settings=False)
                mic.assert_not_called()
                access.assert_called_once()
                self.assertEqual(keyboard.call_count, int(input_required))

    def test_unchanged_permissions_are_not_written_again(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor = PermissionMonitor(version="test", hotkey_backend="passive",
                                        status_path=Path(directory) / "permissions.json")
            with (
                patch("src.permissions._microphone_status", return_value="granted"),
                patch("src.permissions._accessibility_status", return_value="granted") as access,
                patch("src.permissions._input_monitoring_status", return_value="granted"),
                patch.object(monitor, "_atomic_write", wraps=monitor._atomic_write) as write,
            ):
                monitor.refresh()
                for _ in range(10):
                    monitor.refresh()
                self.assertEqual(write.call_count, 1)
                access.return_value = "missing"
                self.assertFalse(monitor.refresh()["all_required_granted"])
                self.assertEqual(write.call_count, 2)

    def test_background_checks_slow_down_but_settings_and_requests_stay_responsive(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor = PermissionMonitor(version="test", hotkey_backend="registered",
                                        status_path=Path(directory) / "permissions.json",
                                        request_path=Path(directory) / "request.json")
            monitor._last_refresh = 0
            with (
                patch("src.permissions.time.monotonic", return_value=2) as clock,
                patch.object(monitor, "_settings_active", return_value=False) as active,
                patch.object(monitor, "refresh") as refresh,
                patch.object(monitor, "_consume_request") as consume,
            ):
                monitor._poll()
                refresh.assert_not_called()
                clock.return_value = 30
                monitor._poll()
                self.assertEqual(refresh.call_count, 1)
                clock.return_value = 32
                active.return_value = True
                monitor._poll()
                self.assertEqual(refresh.call_count, 2)
                active.return_value = False
                monitor.request_path.write_text("{}")
                clock.return_value = 34
                monitor._poll()
                consume.assert_called_once()
                self.assertEqual(refresh.call_count, 3)

    def test_input_monitoring_is_required_for_modifier_only_hotkeys(self):
        self.assertFalse(
            required_permissions_granted(
                "granted", "granted", "missing", input_monitoring_required=True
            )
        )

    def test_registered_hotkey_does_not_require_input_monitoring(self):
        self.assertTrue(
            required_permissions_granted(
                "granted", "granted", "missing", input_monitoring_required=False
            )
        )

    def test_snapshot_identifies_the_exact_current_process(self):
        with tempfile.TemporaryDirectory() as directory:
            monitor = PermissionMonitor(
                version="9.8.7",
                hotkey_backend="registered",
                status_path=Path(directory) / "permissions.json",
                request_path=Path(directory) / "request.json",
            )
            self.assertEqual(monitor.identity["version"], "9.8.7")
            self.assertGreater(monitor.identity["pid"], 1)
            self.assertTrue(monitor.identity["executable_path"])
            self.assertTrue(monitor.identity["executable_fingerprint"])


if __name__ == "__main__":
    unittest.main()
