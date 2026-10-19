import os
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

from Quartz import (
    kCGEventFlagMaskAlternate,
    kCGEventFlagMaskCommand,
    kCGEventFlagsChanged,
    kCGEventKeyDown,
    kCGEventKeyUp,
)

from src.keyboard.listener import (
    KeyboardManager,
    _PassiveGlobalMonitorListener,
    _PassiveTapListener,
    _RegisteredHotKeyListener,
)


class VoiceHotkeyTests(unittest.TestCase):
    def make_manager(self, value):
        manager = KeyboardManager.__new__(KeyboardManager)
        manager._recording_lock = threading.RLock()
        manager.single_fn_hotkey = True
        manager._voice_hotkey_pressed = False
        manager._fn_hotkey_mode = "hold"
        manager.voice_hotkey = value
        manager.voice_hotkey_label = ""
        manager._voice_hotkey_spec = manager._parse_voice_hotkey(value)
        manager.actions = []
        manager.start_recording = lambda: manager.actions.append("start")
        manager.stop_recording = lambda: manager.actions.append("stop")
        manager.toggle_recording = lambda: manager.actions.append("toggle")
        return manager

    def test_right_option_press_and_release(self):
        manager = self.make_manager("right_option")
        self.assertTrue(
            manager._handle_configured_hotkey(
                kCGEventFlagsChanged, 61, kCGEventFlagMaskAlternate
            )
        )
        self.assertTrue(manager._handle_configured_hotkey(kCGEventFlagsChanged, 61, 0))
        self.assertEqual(manager.actions, ["start", "stop"])

    def test_right_command_is_side_specific(self):
        manager = self.make_manager("right_command")
        self.assertFalse(
            manager._handle_configured_hotkey(
                kCGEventFlagsChanged, 55, kCGEventFlagMaskCommand
            )
        )
        self.assertTrue(
            manager._handle_configured_hotkey(
                kCGEventFlagsChanged, 54, kCGEventFlagMaskCommand
            )
        )
        self.assertEqual(manager.actions, ["start"])

    def test_left_option_is_side_specific_and_supported(self):
        manager = self.make_manager("left_option")
        self.assertFalse(
            manager._handle_configured_hotkey(
                kCGEventFlagsChanged, 61, kCGEventFlagMaskAlternate
            )
        )
        self.assertTrue(
            manager._handle_configured_hotkey(
                kCGEventFlagsChanged, 58, kCGEventFlagMaskAlternate
            )
        )
        self.assertEqual(manager.actions, ["start"])

    def test_toggle_mode_starts_and_stops_on_successive_presses(self):
        manager = self.make_manager("right_option")
        manager._fn_hotkey_mode = "toggle"
        manager.is_recording = False

        def toggle():
            manager.actions.append("toggle")
            manager.is_recording = not manager.is_recording

        manager.toggle_recording = toggle
        for _ in range(2):
            self.assertTrue(
                manager._handle_configured_hotkey(
                    kCGEventFlagsChanged, 61, kCGEventFlagMaskAlternate
                )
            )
            self.assertTrue(
                manager._handle_configured_hotkey(kCGEventFlagsChanged, 61, 0)
            )
        self.assertEqual(manager.actions, ["toggle", "toggle"])

    def test_recorded_combo_uses_keycode_and_modifiers(self):
        manager = self.make_manager("keycode:49;mods:command+option")
        flags = kCGEventFlagMaskCommand | kCGEventFlagMaskAlternate
        self.assertTrue(manager._handle_configured_hotkey(kCGEventKeyDown, 49, flags))
        self.assertTrue(manager._handle_configured_hotkey(kCGEventKeyUp, 49, flags))
        self.assertEqual(manager.actions, ["start", "stop"])

    def test_recorded_combo_does_not_trigger_without_modifier(self):
        manager = self.make_manager("keycode:49;mods:command")
        self.assertFalse(manager._handle_configured_hotkey(kCGEventKeyDown, 49, 0))
        self.assertEqual(manager.actions, [])

    def test_registered_combo_rejects_unknown_or_missing_modifiers(self):
        manager = self.make_manager("keycode:49;mods:control")

        self.assertIsNone(manager._parse_voice_hotkey("keycode:49;mods:"))
        self.assertIsNone(manager._parse_voice_hotkey("keycode:49;mods:hyper"))
        self.assertIsNone(
            manager._parse_voice_hotkey("keycode:49;mods:control+control")
        )
        self.assertIsNone(manager._parse_voice_hotkey("keycode:999;mods:control"))
        self.assertIsNone(
            manager._parse_voice_hotkey("keycode:49;keycode:40;mods:control")
        )
        self.assertIsNone(
            manager._parse_voice_hotkey("keycode:49;mods:control;extra:value")
        )

    def test_registered_combo_supports_function_modifier(self):
        manager = self.make_manager("keycode:49;mods:function")

        self.assertEqual(manager._voice_hotkey_spec["mask"], 1 << 23)
        self.assertEqual(_RegisteredHotKeyListener._CARBON_MODIFIERS["function"], 1 << 23)

    def test_backend_is_derived_from_shortcut_kind_but_off_is_respected(self):
        combo = {"kind": "key"}
        modifier_only = {"kind": "modifier"}

        self.assertEqual(
            KeyboardManager._backend_for_hotkey(combo, "passive"), "registered"
        )
        self.assertEqual(
            KeyboardManager._backend_for_hotkey(modifier_only, "registered"),
            "passive",
        )
        self.assertEqual(KeyboardManager._backend_for_hotkey(combo, "off"), "off")

    def test_shortcut_capture_lock_temporarily_passes_current_hotkey_through(self):
        manager = self.make_manager("right_option")
        manager._suppress_vks = {61}
        manager._suppress_modifier_mask = kCGEventFlagMaskAlternate
        manager._listener = None
        with tempfile.TemporaryDirectory() as directory:
            lock = Path(directory) / ".shortcut-capture"
            lock.write_text(str(os.getpid()), encoding="utf-8")
            manager._shortcut_capture_lock_file = lock
            manager._shortcut_capture_checked_at = 0.0
            manager._shortcut_capture_is_active = False
            event = object()
            with (
                patch("Quartz.CGEventGetIntegerValueField", return_value=61),
                patch("Quartz.CGEventGetFlags", return_value=kCGEventFlagMaskAlternate),
            ):
                self.assertIs(
                    manager._darwin_intercept(kCGEventFlagsChanged, event),
                    event,
                )
        self.assertEqual(manager.actions, [])

    def test_passive_listener_never_swallows_observed_event(self):
        observed = []
        listener = _PassiveTapListener(
            lambda event_type, event: observed.append((event_type, event)) or None
        )
        event = object()

        returned = listener._deliver(None, kCGEventFlagsChanged, event, None)

        self.assertIs(returned, event)
        self.assertEqual(observed, [(kCGEventFlagsChanged, event)])

    def test_appkit_monitor_has_no_suppressing_return_value(self):
        observed = []
        listener = _PassiveGlobalMonitorListener(observed.append)
        event = object()

        self.assertIsNone(listener._deliver(event))
        self.assertEqual(observed, [event])

    def test_appkit_monitor_drives_right_option_press_and_release(self):
        manager = self.make_manager("right_option")
        manager.is_recording = False
        manager._suppress_vks = {61}
        manager._shortcut_capture_active = lambda **_kwargs: False

        class Event:
            def __init__(self, event_type, flags):
                self._event_type = event_type
                self._flags = flags

            def type(self):
                return self._event_type

            def keyCode(self):
                return 61

            def modifierFlags(self):
                return self._flags

        manager._nsevent_intercept(Event(kCGEventFlagsChanged, kCGEventFlagMaskAlternate))
        manager._nsevent_intercept(Event(kCGEventFlagsChanged, 0))

        self.assertEqual(manager.actions, ["start", "stop"])

    def test_registered_hotkey_dispatches_only_press_and_release(self):
        states = []
        listener = _RegisteredHotKeyListener(
            {"kind": "key", "vk": 49, "modifiers": ["control", "option"]},
            states.append,
        )

        self.assertEqual(listener._dispatch_kind(listener._EVENT_HOTKEY_PRESSED), 0)
        self.assertEqual(listener._dispatch_kind(999), 0)
        self.assertEqual(listener._dispatch_kind(listener._EVENT_HOTKEY_RELEASED), 0)

        self.assertEqual(states, [True, False])

    def test_registered_hotkey_is_ignored_while_settings_capture_is_active(self):
        manager = self.make_manager("keycode:49;mods:control+option")
        manager._shortcut_capture_active = lambda **_kwargs: True

        manager._set_registered_hotkey_pressed(True)
        manager._set_registered_hotkey_pressed(False)

        self.assertEqual(manager.actions, [])

    def test_passive_tap_repairs_disabled_tap(self):
        listener = _PassiveTapListener(lambda *_args: None)
        listener.tap_ref = object()
        with (
            patch("Quartz.CGEventTapIsEnabled", side_effect=[False, True]),
            patch("Quartz.CGEventTapEnable") as enable,
        ):
            self.assertTrue(listener._repair_if_disabled())

        enable.assert_called_once_with(listener.tap_ref, True)
        self.assertEqual(listener.recovery_count, 1)

    def test_recovery_clears_lost_release_in_toggle_mode(self):
        manager = self.make_manager("right_option")
        manager._hotkey_mode = "toggle"
        manager.is_recording = False
        manager._handle_configured_hotkey(kCGEventFlagsChanged, 61, kCGEventFlagMaskAlternate)
        manager._recover_hotkey_state()
        manager._handle_configured_hotkey(kCGEventFlagsChanged, 61, kCGEventFlagMaskAlternate)
        self.assertEqual(manager.actions, ["toggle", "toggle"])

    def test_poll_reconciles_missing_release_without_new_key_event(self):
        manager = self.make_manager("right_option")
        manager._handle_configured_hotkey(kCGEventFlagsChanged, 61, kCGEventFlagMaskAlternate)
        with (patch("Quartz.CGEventSourceKeyState", return_value=False),
              patch("Quartz.CGEventSourceFlagsState", return_value=0)):
            manager._reconcile_hotkey_release()
            self.assertEqual(manager.actions, ["start"])
            manager._reconcile_hotkey_release()
        self.assertEqual(manager.actions, ["start", "stop"])

    def test_modifier_hold_survives_key_state_false_while_flags_still_down(self):
        manager = self.make_manager("right_option")
        manager._handle_configured_hotkey(kCGEventFlagsChanged, 61, kCGEventFlagMaskAlternate)
        with (patch("Quartz.CGEventSourceKeyState", return_value=False),
              patch("Quartz.CGEventSourceFlagsState", return_value=kCGEventFlagMaskAlternate)):
            for _ in range(5):
                manager._reconcile_hotkey_release()
        self.assertEqual(manager.actions, ["start"])
        manager._handle_configured_hotkey(kCGEventFlagsChanged, 61, 0)
        self.assertEqual(manager.actions, ["start", "stop"])

    def test_transient_poll_disagreement_does_not_stop_hold(self):
        manager = self.make_manager("right_option")
        manager._handle_configured_hotkey(kCGEventFlagsChanged, 61, kCGEventFlagMaskAlternate)
        with (patch("Quartz.CGEventSourceKeyState", return_value=False),
              patch("Quartz.CGEventSourceFlagsState", side_effect=[0, kCGEventFlagMaskAlternate, 0])):
            for _ in range(3):
                manager._reconcile_hotkey_release()
        self.assertEqual(manager.actions, ["start"])

    def test_releasing_right_option_while_left_option_held_stops_immediately(self):
        manager = self.make_manager("right_option")
        manager._handle_configured_hotkey(kCGEventFlagsChanged, 61, kCGEventFlagMaskAlternate | 0x40)
        manager._handle_configured_hotkey(kCGEventFlagsChanged, 61, kCGEventFlagMaskAlternate | 0x20)
        self.assertEqual(manager.actions, ["start", "stop"])

    def test_combo_poll_does_not_treat_remaining_modifier_as_main_key_held(self):
        manager = self.make_manager("keycode:49;mods:command")
        manager._handle_configured_hotkey(kCGEventKeyDown, 49, kCGEventFlagMaskCommand)
        with (patch("Quartz.CGEventSourceKeyState", return_value=False),
              patch("Quartz.CGEventSourceFlagsState", return_value=kCGEventFlagMaskCommand)):
            manager._reconcile_hotkey_release()
            manager._reconcile_hotkey_release()
        self.assertEqual(manager.actions, ["start", "stop"])

    def test_all_modifier_shortcuts_use_their_own_flag_and_side(self):
        for key, vk, side, other in [
            ("left_option", 58, 0x20, 0x40), ("right_option", 61, 0x40, 0x20),
            ("left_command", 55, 0x08, 0x10), ("right_command", 54, 0x10, 0x08),
            ("left_control", 59, 0x01, 0x2000), ("right_control", 62, 0x2000, 0x01),
            ("fn", 63, 0, 0),
        ]:
            with self.subTest(key=key):
                manager = self.make_manager(key)
                mask = manager._voice_hotkey_spec["mask"]
                manager._handle_configured_hotkey(kCGEventFlagsChanged, vk, mask | side)
                with (patch("Quartz.CGEventSourceKeyState", return_value=False),
                      patch("Quartz.CGEventSourceFlagsState", return_value=mask | side)):
                    for _ in range(3):
                        manager._reconcile_hotkey_release()
                self.assertEqual(manager.actions, ["start"])
                manager._handle_configured_hotkey(kCGEventFlagsChanged, vk, (mask | other) if other else 0)
                self.assertEqual(manager.actions, ["start", "stop"])

    def test_poll_miss_does_not_leak_into_next_hold(self):
        manager = self.make_manager("right_option")
        with (patch("Quartz.CGEventSourceKeyState", return_value=False),
              patch("Quartz.CGEventSourceFlagsState", return_value=0)):
            for _ in range(2):
                manager._handle_configured_hotkey(kCGEventFlagsChanged, 61, kCGEventFlagMaskAlternate)
                manager._reconcile_hotkey_release()
                self.assertEqual(manager.actions[-1], "start")
                manager._handle_configured_hotkey(kCGEventFlagsChanged, 61, 0)
        self.assertEqual(manager.actions, ["start", "stop", "start", "stop"])

    def test_failed_tap_enable_is_not_reported_as_recovered(self):
        listener = _PassiveTapListener(lambda *_: None)
        listener.tap_ref = object()
        with patch("Quartz.CGEventTapIsEnabled", return_value=False), patch("Quartz.CGEventTapEnable"):
            with self.assertRaises(RuntimeError):
                listener._repair_if_disabled()

    def test_registered_conflict_shows_actionable_warning(self):
        manager = self.make_manager("keycode:49;mods:control+option")
        manager._hotkey_backend = "registered"
        manager._shortcut_capture_active = lambda **_kwargs: False
        manager.show_warning = MagicMock()
        registered = MagicMock()
        registered.start.side_effect = RuntimeError("eventHotKeyExistsErr")

        with (
            patch("src.keyboard.listener.sys.platform", "darwin"),
            patch(
                "src.keyboard.listener._RegisteredHotKeyListener",
                return_value=registered,
            ),
        ):
            manager.start_listening()

        manager.show_warning.assert_called_once_with(
            "组合键注册失败；请在设置中选择其他快捷键"
        )

    def test_passive_permission_failure_falls_back_and_explains_fix(self):
        manager = self.make_manager("right_option")
        manager._hotkey_backend = "passive"
        manager._shortcut_capture_active = lambda **_kwargs: False
        manager._darwin_intercept = MagicMock()
        manager._nsevent_intercept = MagicMock()
        manager._suppress_vks = {61}
        manager.show_warning = MagicMock()

        passive = MagicMock()
        passive.__enter__.side_effect = RuntimeError("tap denied")
        fallback = MagicMock()
        fallback.start.side_effect = RuntimeError("monitor denied")

        with (
            patch("src.keyboard.listener.sys.platform", "darwin"),
            patch(
                "src.keyboard.listener._PassiveTapListener", return_value=passive
            ),
            patch(
                "src.keyboard.listener._PassiveGlobalMonitorListener",
                return_value=fallback,
            ),
            patch(
                "PyObjCTools.AppHelper.callAfter",
                side_effect=lambda callback: callback(),
            ),
        ):
            manager.start_listening()

        fallback.start.assert_called_once_with()
        manager.show_warning.assert_called_once_with(
            "请为 Voice Input 开启输入监控权限"
        )


if __name__ == "__main__":
    unittest.main()
