import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

from src.ui import status_bar


class SettingsLaunchTests(unittest.TestCase):
    def test_menu_preference_applies_without_restarting(self):
        controller = status_bar.StatusBarController.__new__(status_bar.StatusBarController)
        controller._status_item = None
        with patch.object(status_bar.AppHelper, "callAfter", side_effect=lambda f: f()), \
                patch.object(status_bar, "NSApplication"), \
                patch.object(status_bar, "_menu_bar_enabled", return_value=True) as enabled, \
                patch.object(controller, "_install_status_item") as install, \
                patch.object(controller, "_schedule_status_health_check") as schedule, \
                patch.object(status_bar, "NSStatusBar") as bar:
            controller.apply_preferences()
            install.assert_called_once()
            schedule.assert_called_once()
            item = controller._status_item = MagicMock()
            enabled.return_value = False
            controller.apply_preferences()
            bar.systemStatusBar().removeStatusItem_.assert_called_once_with(item)
            self.assertIsNone(controller._status_item)

    def test_disabled_menu_stops_its_health_check(self):
        controller = status_bar.StatusBarController.__new__(status_bar.StatusBarController)
        controller._status_health_check_scheduled = False
        with patch.object(status_bar.AppHelper, "callLater") as later, \
                patch.object(status_bar, "_menu_bar_enabled", return_value=False):
            controller._schedule_status_health_check()
            self.assertEqual(later.call_args.args[0], 30.0)
            later.call_args.args[1]()
            self.assertFalse(controller._status_health_check_scheduled)
            self.assertEqual(later.call_count, 1)

    def test_main_relaunch_reattaches_to_verified_settings_helper(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / ".settings-instance").write_text("24680")
            binary = root / "Contents/Helpers/VeyqaSettings.app/Contents/MacOS/VeyqaSettings"
            with patch.object(status_bar, "_settings_process", None), \
                    patch.object(status_bar, "IS_FROZEN", True), \
                    patch.object(status_bar, "DATA_DIR", root), \
                    patch.object(status_bar, "LOG_DIR", root), \
                    patch.object(status_bar, "app_bundle_path", return_value=root), \
                    patch.object(status_bar, "NSRunningApplication") as running, \
                    patch.object(status_bar.os, "kill") as signal, \
                    patch.object(status_bar.subprocess, "Popen") as spawn:
                running.runningApplicationWithProcessIdentifier_.return_value.executableURL.return_value.path.return_value = str(binary)
                self.assertEqual(status_bar._launch_settings_window(), 24680)
                signal.assert_called_once_with(24680, status_bar.signal.SIGUSR1)
                running.runningApplicationWithProcessIdentifier_.return_value.unhide.assert_called_once()
                running.runningApplicationWithProcessIdentifier_.return_value.activateWithOptions_.assert_called_once()
                spawn.assert_not_called()

    def test_recovery_screen_is_reachable_and_settings_closes_after_last_window(self):
        swift = (Path(__file__).resolve().parents[1] / "native_settings/VoiceInputSettings.swift").read_text()
        self.assertIn("RecoverySettingsView(model: model)", swift)
        self.assertIn("applicationShouldTerminateAfterLastWindowClosed", swift)
        self.assertIn("guard NSApp.isActive || model.applyingSettings else { return }", swift)
        self.assertIn("guard diagnosticsGate.changed(contents) else { return }", swift)
        self.assertIn("guard recoveriesGate.changed(contents) else { return }", swift)
        self.assertIn("RecoveryFile.delete(ids: ids, at: url)", swift)
        self.assertNotIn("persistRecoveries()", swift)
        self.assertIn('Window("语音输入设置", id: "settings")', swift)
        self.assertIn('openWindow(id: "settings")', swift)
        self.assertIn("NSApp.windows.filter(isSettingsWindow)", swift)
        self.assertIn("NSWindow.willCloseNotification", swift)
        self.assertIn("NSApp.unhide(nil)", swift)

    def test_status_log_rotates_and_logging_failure_is_nonfatal(self):
        with tempfile.TemporaryDirectory() as directory:
            log_dir = Path(directory)
            log_path = log_dir / "settings_ui.log"
            log_path.write_bytes(b"x" * status_bar._STATUS_LOG_MAX_BYTES)
            with patch.object(status_bar, "LOG_DIR", log_dir):
                status_bar._write_status_log("after rotation")

            self.assertTrue((log_dir / "settings_ui.log.1").exists())
            self.assertIn("after rotation", log_path.read_text(encoding="utf-8"))

        with patch.object(status_bar, "LOG_DIR", Path("/dev/null/not-a-directory")):
            status_bar._write_status_log("must not raise")

    def test_warming_settings_helper_is_not_signalled_before_ready_handshake(self):
        process = MagicMock()
        process.pid = 24680
        process.poll.return_value = None

        with tempfile.TemporaryDirectory() as directory:
            with (
                patch.object(status_bar, "DATA_DIR", Path(directory)),
                patch.object(status_bar.os, "kill") as send_signal,
            ):
                self.assertTrue(status_bar._activate_settings_process(process))

        send_signal.assert_not_called()

    def test_ready_settings_helper_is_signalled(self):
        process = MagicMock()
        process.pid = 24680
        process.poll.return_value = None

        with tempfile.TemporaryDirectory() as directory:
            Path(directory, ".settings-instance").write_text("24680", encoding="utf-8")
            with (
                patch.object(status_bar, "DATA_DIR", Path(directory)),
                patch.object(status_bar.os, "kill") as send_signal,
                patch.object(status_bar, "NSRunningApplication") as running_app,
            ):
                running_app.runningApplicationWithProcessIdentifier_.return_value = None
                self.assertTrue(status_bar._activate_settings_process(process))

        send_signal.assert_called_once_with(process.pid, status_bar.signal.SIGUSR1)

    def test_dock_can_be_hidden_and_restored_without_restarting(self):
        controller = status_bar.StatusBarController.__new__(status_bar.StatusBarController)
        controller._status_item = MagicMock()
        app = MagicMock()
        app.activationPolicy.return_value = status_bar.NSApplicationActivationPolicyRegular
        with patch.dict(status_bar.os.environ, {"SHOW_DOCK_ICON": "false"}):
            controller._apply_activation_policy(app)
        app.setActivationPolicy_.assert_called_once_with(status_bar.NSApplicationActivationPolicyAccessory)
        app.reset_mock()
        app.activationPolicy.return_value = status_bar.NSApplicationActivationPolicyAccessory
        with patch.dict(status_bar.os.environ, {"SHOW_DOCK_ICON": "true"}):
            controller._apply_activation_policy(app)
        app.setActivationPolicy_.assert_called_once_with(status_bar.NSApplicationActivationPolicyRegular)

    def test_hidden_dock_keeps_menu_entry_even_with_conflicting_config(self):
        with patch.dict(status_bar.os.environ, {"SHOW_DOCK_ICON": "false", "SHOW_MENU_BAR_ICON": "false"}, clear=True):
            self.assertTrue(status_bar._menu_bar_enabled())

    def test_failed_menu_creation_keeps_dock_as_recovery_entry(self):
        controller = status_bar.StatusBarController.__new__(status_bar.StatusBarController)
        app = MagicMock()
        app.activationPolicy.return_value = status_bar.NSApplicationActivationPolicyAccessory
        with patch.dict(status_bar.os.environ, {"SHOW_DOCK_ICON": "false"}):
            for item in [None, MagicMock()]:
                if item is not None:
                    item.button.return_value = None
                controller._status_item = item
                controller._apply_activation_policy(app)
                app.setActivationPolicy_.assert_called_with(status_bar.NSApplicationActivationPolicyRegular)

    def test_unchanged_activation_policy_does_not_refresh_dock(self):
        controller = status_bar.StatusBarController.__new__(status_bar.StatusBarController)
        controller._status_item = MagicMock()
        app = MagicMock()
        app.activationPolicy.return_value = status_bar.NSApplicationActivationPolicyAccessory
        with patch.dict(status_bar.os.environ, {"SHOW_DOCK_ICON": "false"}):
            controller._apply_activation_policy(app)
        app.setActivationPolicy_.assert_not_called()

    def test_menu_bar_is_disabled_by_default(self):
        with patch.dict(status_bar.os.environ, {}, clear=True):
            self.assertFalse(status_bar._menu_bar_enabled())
        with patch.dict(status_bar.os.environ, {"SHOW_MENU_BAR_ICON": "true"}, clear=True):
            self.assertTrue(status_bar._menu_bar_enabled())

    def test_dock_reopen_opens_settings(self):
        delegate = status_bar._ApplicationDelegate.alloc().init()
        delegate.controller = MagicMock()

        self.assertTrue(
            delegate.applicationShouldHandleReopen_hasVisibleWindows_(None, False)
        )

        delegate.controller._open_settings.assert_called_once_with()

    def test_settings_copy_has_no_enterprise_compatibility_badges(self):
        root = Path(__file__).resolve().parents[1]
        swift = (root / "native_settings" / "VoiceInputSettings.swift").read_text(
            encoding="utf-8"
        )
        fallback = (root / "tools" / "settings_ui.py").read_text(encoding="utf-8")
        self.assertNotIn("企业兼容", swift)
        self.assertNotIn("企业电脑推荐", fallback)
        self.assertNotIn("Karabiner 用户", swift)
        self.assertIn("重新打开 App", swift)

    def test_status_controller_has_no_recording_callback(self):
        with self.assertRaises(TypeError):
            status_bar.StatusBarController(on_toggle_recording=lambda: None)

    def test_new_helper_is_not_signalled_before_handler_is_ready(self):
        process = MagicMock()
        process.pid = 43210
        process.wait.return_value = 0
        process.poll.return_value = None

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "native_settings").mkdir()
            log_dir = root / "data" / "logs"
            data_dir = root / "data"
            with (
                patch.object(status_bar, "_settings_process", None),
                patch.object(status_bar, "IS_FROZEN", True),
                patch.object(status_bar, "RESOURCE_ROOT", root),
                patch.object(status_bar, "LOG_DIR", log_dir),
                patch.object(status_bar, "DATA_DIR", data_dir),
                patch.object(status_bar, "app_bundle_path", return_value=None),
                patch.object(status_bar.subprocess, "Popen", return_value=process),
                patch.object(status_bar, "_activate_settings_process") as activate,
            ):
                pid = status_bar._launch_settings_window()

        self.assertEqual(pid, process.pid)
        activate.assert_not_called()

    def test_status_item_left_click_opens_settings(self):
        handler = status_bar._MenuActionHandler.alloc().init()

        with (
            patch.object(status_bar, "_current_event_type", return_value=2),
            patch.object(status_bar, "_launch_settings_window") as launch,
        ):
            handler.handleStatusItem_(None)

        launch.assert_called_once()

    def test_status_item_right_click_opens_settings(self):
        handler = status_bar._MenuActionHandler.alloc().init()

        with (
            patch.object(
                status_bar,
                "_current_event_type",
                return_value=status_bar.NSEventTypeRightMouseUp,
            ),
            patch.object(status_bar, "_launch_settings_window") as launch,
        ):
            handler.handleStatusItem_(None)

        launch.assert_called_once()

    @patch.dict(status_bar.os.environ, {"SHOW_MENU_BAR_ICON": "true"})
    def test_health_check_recreates_a_lost_status_item(self):
        with patch.object(status_bar.StatusBarController, "_load_custom_icons"):
            controller = status_bar.StatusBarController()
        controller._status_item = None

        with patch.object(status_bar.AppHelper, "callLater") as schedule:
            controller._schedule_status_health_check()
        health_check = schedule.call_args.args[1]

        with (
            patch.object(controller, "_install_status_item") as install,
            patch.object(controller, "_schedule_status_health_check") as reschedule,
            patch.object(status_bar, "NSApplication"),
            patch.object(status_bar, "_write_status_log"),
        ):
            health_check()

        install.assert_called_once_with()
        reschedule.assert_called_once_with()

    @patch.dict(status_bar.os.environ, {"SHOW_MENU_BAR_ICON": "true"})
    def test_health_check_restores_hidden_status_item(self):
        with patch.object(status_bar.StatusBarController, "_load_custom_icons"):
            controller = status_bar.StatusBarController()
        item = MagicMock()
        item.button.return_value = MagicMock()
        item.isVisible.return_value = False
        controller._status_item = item

        with patch.object(status_bar.AppHelper, "callLater") as schedule:
            controller._schedule_status_health_check()
        health_check = schedule.call_args.args[1]

        with (
            patch.object(controller, "_refresh") as refresh,
            patch.object(controller, "_schedule_status_health_check"),
            patch.object(status_bar, "NSApplication"),
            patch.object(status_bar, "_write_status_log"),
        ):
            health_check()

        item.setVisible_.assert_called_once_with(True)
        refresh.assert_not_called()


if __name__ == "__main__":
    unittest.main()
