import unittest
import tempfile
import plistlib
from pathlib import Path
from unittest.mock import MagicMock, patch

from src import login_item


class LoginItemTests(unittest.TestCase):
    def test_local_signing_uses_separate_service_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / "Veyqa Voice.app"
            (bundle / "Contents").mkdir(parents=True)
            (bundle / "Contents/Info.plist").write_bytes(plistlib.dumps({"VeyqaSigningProfile": "local"}))
            with patch.object(login_item, "app_bundle_path", return_value=bundle):
                self.assertEqual(login_item._agent_plist_name(), "com.wjcdev.veyqa.local.agent.plist")

    def test_local_migration_unregisters_old_identity_before_registering(self):
        events = []
        old, current = MagicMock(), MagicMock()
        old.status.return_value = 1
        old.unregisterAndReturnError_.side_effect = lambda _: (events.append("unregister_old") is None, None)
        current.status.side_effect = [0, 1]
        current.registerAndReturnError_.side_effect = lambda _: (events.append("register_local") is None, None)
        framework = MagicMock()
        framework.SMAppService.agentServiceWithPlistName_.side_effect = lambda name: (
            old if name == login_item.AGENT_PLIST_NAME else current)
        with (
            patch.object(login_item, "IS_FROZEN", True),
            patch.object(login_item, "_agent_plist_name", return_value=login_item.LOCAL_AGENT_PLIST_NAME),
            patch.object(login_item, "_registration_identity", return_value=""),
            patch.dict("sys.modules", {"ServiceManagement": framework}),
            patch.object(login_item, "_retire_legacy_agents"),
            patch.object(login_item, "_legacy_app_is_running", return_value=False),
        ):
            self.assertEqual(login_item.sync_login_item(True).status, "enabled")
        self.assertEqual(events, ["unregister_old", "register_local"])

    def test_upgrade_refreshes_stale_enabled_registration_once(self):
        service = MagicMock()
        service.status.return_value = 1
        service.registerAndReturnError_.return_value = (True, None)
        service.unregisterAndReturnError_.return_value = (True, None)
        framework = MagicMock()
        framework.SMAppService.agentServiceWithPlistName_.return_value = service
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(login_item, "IS_FROZEN", True),
            patch.object(login_item, "_registration_identity", return_value="app:202:new-inode"),
            patch.object(login_item, "REGISTRATION_FILE", Path(directory) / "registration.json"),
            patch.dict("sys.modules", {"ServiceManagement": framework}),
            patch.object(login_item, "_retire_legacy_agents"),
            patch.object(login_item, "_legacy_app_is_running", return_value=False),
        ):
            self.assertTrue(login_item.sync_login_item(True).changed)
            self.assertFalse(login_item.sync_login_item(True).changed)
        service.unregisterAndReturnError_.assert_called_once_with(None)
        service.registerAndReturnError_.assert_called_once_with(None)

    def test_upgrade_does_not_register_over_failed_unregister(self):
        service = MagicMock()
        service.status.return_value = 1
        service.unregisterAndReturnError_.return_value = (False, "unavailable")
        framework = MagicMock()
        framework.SMAppService.agentServiceWithPlistName_.return_value = service
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(login_item, "IS_FROZEN", True),
            patch.object(login_item, "_registration_identity", return_value="new"),
            patch.object(login_item, "REGISTRATION_FILE", Path(directory) / "registration.json"),
            patch.dict("sys.modules", {"ServiceManagement": framework}),
        ):
            self.assertEqual(login_item.sync_login_item(True).error, "unavailable")
        service.registerAndReturnError_.assert_not_called()

    def test_manual_launch_clears_explicit_supervisor_pause(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / ".supervisor-paused"
            marker.write_text("paused-by-user", encoding="utf-8")
            with patch.object(login_item, "SUPERVISOR_PAUSE_FILE", marker):
                login_item.resume_supervisor_for_manual_launch(["--background-login"])
            self.assertFalse(marker.exists())

    def test_supervised_launch_preserves_explicit_pause(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / ".supervisor-paused"
            marker.write_text("paused-by-user", encoding="utf-8")
            with patch.object(login_item, "SUPERVISOR_PAUSE_FILE", marker):
                login_item.resume_supervisor_for_manual_launch(["--supervised"])
            self.assertTrue(marker.exists())

    def test_isolated_runtime_can_disable_external_registration(self):
        with (
            patch.dict("os.environ", {"VOICE_INPUT_DISABLE_LOGIN_SYNC": "true"}),
            patch.object(login_item, "IS_FROZEN", True),
        ):
            result = login_item.sync_login_item(True)

        self.assertEqual(result.status, "disabled_for_test")

    def test_source_run_never_registers_external_service(self):
        with patch.object(login_item, "IS_FROZEN", False):
            result = login_item.sync_login_item(True)

        self.assertEqual(result.status, "source_run")
        self.assertFalse(result.changed)

    def test_legacy_agents_retire_only_after_modern_service_is_enabled(self):
        service = MagicMock()
        service.status.side_effect = [0, 1]
        service.registerAndReturnError_.return_value = (True, None)
        framework = MagicMock()
        framework.SMAppService.agentServiceWithPlistName_.return_value = service

        with (
            patch.object(login_item, "IS_FROZEN", True),
            patch.dict("sys.modules", {"ServiceManagement": framework}),
            patch.object(login_item, "_retire_legacy_agents") as retire,
            patch.object(login_item, "_legacy_app_is_running", return_value=False),
        ):
            result = login_item.sync_login_item(True)

        self.assertEqual(result.status, "enabled")
        self.assertTrue(result.changed)
        retire.assert_called_once_with()

    def test_running_legacy_app_is_reported_instead_of_silent_duplicate(self):
        service = MagicMock()
        service.status.return_value = 1
        framework = MagicMock()
        framework.SMAppService.agentServiceWithPlistName_.return_value = service

        with (
            patch.object(login_item, "IS_FROZEN", True),
            patch.dict("sys.modules", {"ServiceManagement": framework}),
            patch.object(login_item, "_retire_legacy_agents"),
            patch.object(login_item, "_legacy_app_is_running", return_value=True),
        ):
            result = login_item.sync_login_item(True)

        self.assertEqual(result.status, "enabled_legacy_running")
        self.assertIn("旧版 Voice Input", result.error)

    def test_registration_error_preserves_legacy_agents(self):
        service = MagicMock()
        service.status.return_value = 0
        service.registerAndReturnError_.return_value = (False, "not signed")
        framework = MagicMock()
        framework.SMAppService.agentServiceWithPlistName_.return_value = service

        with (
            patch.object(login_item, "IS_FROZEN", True),
            patch.dict("sys.modules", {"ServiceManagement": framework}),
            patch.object(login_item, "_retire_legacy_agents") as retire,
        ):
            result = login_item.sync_login_item(True)

        self.assertEqual(result.error, "not signed")
        retire.assert_not_called()


if __name__ == "__main__":
    unittest.main()
