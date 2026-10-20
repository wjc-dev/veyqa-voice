import threading
from unittest.mock import patch

import pytest
from Quartz import (
    kCGEventFlagsChanged, kCGEventKeyDown, kCGEventKeyUp,
    kCGEventFlagMaskCommand, kCGEventFlagMaskShift,
)

from src.keyboard.listener import KeyboardManager


class ManualTimer:
    timers = []

    def __init__(self, delay, callback):
        self.delay, self.callback = delay, callback
        self.cancelled = False
        self.timers.append(self)

    def start(self):
        pass

    def cancel(self):
        self.cancelled = True

    def fire(self):
        # Also simulate a callback already enqueued when cancellation happens.
        self.callback()


@pytest.fixture
def manager():
    ManualTimer.timers = []
    value = KeyboardManager.__new__(KeyboardManager)
    value._recording_lock = threading.RLock()
    value._hotkey_mode = "toggle"
    value._voice_hotkey_pressed = False
    value._voice_hotkey_spec = value._parse_voice_hotkey("left_command")
    value.is_recording = False
    value._shortcut_capture_active = lambda **kw: False
    value.actions = []
    def action(name, recording):
        value.actions.append(name)
        value.is_recording = recording
    value.start_recording = lambda: action("start", True)
    value.stop_recording = lambda: action("stop", False)
    value.cancel_recording = lambda: action("cancel", False)
    value.toggle_recording = lambda: action("toggle", not value.is_recording)
    with patch("src.keyboard.modifier_gesture.threading.Timer", ManualTimer), \
            patch("Quartz.CGEventSourceKeyState",
                  side_effect=lambda source, code: code in getattr(value, "_other_keys_down", set())):
        yield value
    if value._modifier_gesture:
        value._modifier_gesture.reset()


def command(manager, down=True, extra=0):
    manager._handle_configured_hotkey(
        kCGEventFlagsChanged, 55, (kCGEventFlagMaskCommand | 0x08 | extra) if down else extra)


@pytest.mark.parametrize("key", [0, 8, 9, 12, 48])  # A, C, V, Q, Tab
@pytest.mark.parametrize("release_modifier_first", [False, True])
def test_command_shortcuts_never_toggle_recording(manager, key, release_modifier_first):
    command(manager)
    assert manager.actions == []
    manager._handle_configured_hotkey(kCGEventKeyDown, key, kCGEventFlagMaskCommand)
    if release_modifier_first:
        command(manager, False)
    manager._handle_configured_hotkey(kCGEventKeyUp, key, 0)
    if not release_modifier_first:
        command(manager, False)
    assert manager.actions == []
    command(manager)
    command(manager, False)
    assert manager.actions == ["toggle"]


def test_chord_does_not_stop_an_existing_toggle_recording(manager):
    manager.is_recording = True
    command(manager)
    manager._handle_configured_hotkey(kCGEventKeyDown, 0, kCGEventFlagMaskCommand)
    command(manager, False)
    assert manager.is_recording
    assert manager.actions == []


def test_keys_already_down_and_extra_modifiers_do_not_trigger(manager):
    manager._handle_configured_hotkey(kCGEventKeyDown, 0, 0)
    command(manager)
    command(manager, False)
    manager._handle_configured_hotkey(kCGEventKeyUp, 0, 0)
    command(manager, extra=kCGEventFlagMaskShift)
    command(manager, False)
    assert manager.actions == []


def test_hold_chord_before_threshold_never_opens_microphone(manager):
    manager._hotkey_mode = "hold"
    command(manager)
    timer = ManualTimer.timers[-1]
    assert manager.actions == []
    manager._handle_configured_hotkey(kCGEventKeyDown, 0, kCGEventFlagMaskCommand)
    timer.fire()
    command(manager, False)
    assert timer.cancelled
    assert manager.actions == []


def test_late_chord_cancels_tentative_hold_without_committing_text(manager):
    manager._hotkey_mode = "hold"
    command(manager)
    ManualTimer.timers[-1].fire()
    assert manager.actions == ["start"]
    manager._handle_configured_hotkey(kCGEventKeyDown, 0, kCGEventFlagMaskCommand)
    command(manager, False)
    assert manager.actions == ["start", "cancel"]


def test_short_hold_and_old_timer_cannot_trigger_a_later_gesture(manager):
    manager._hotkey_mode = "hold"
    command(manager)
    old_timer = ManualTimer.timers[-1]
    command(manager, False)
    command(manager)
    old_timer.fire()
    assert manager.actions == []
    ManualTimer.timers[-1].fire()
    command(manager, False)
    assert manager.actions == ["start", "stop"]


def test_capture_start_before_hold_threshold_prevents_recording(manager):
    manager._hotkey_mode = "hold"
    command(manager)
    manager._shortcut_capture_active = lambda **kw: True
    ManualTimer.timers[-1].fire()
    command(manager, False)
    assert manager.actions == []


def test_poll_release_does_not_invent_a_toggle_tap(manager):
    command(manager)
    with patch("Quartz.CGEventSourceKeyState", return_value=False), \
            patch("Quartz.CGEventSourceFlagsState", return_value=0):
        manager._reconcile_hotkey_release()
        manager._reconcile_hotkey_release()
    assert manager.actions == []


def test_listener_recovery_cancels_pending_timer(manager):
    manager._hotkey_mode = "hold"
    command(manager)
    timer = ManualTimer.timers[-1]
    manager._recover_hotkey_state()
    timer.fire()
    assert manager.actions == []


def test_opposite_command_and_shift_chords_do_not_toggle(manager):
    command(manager, extra=0x10)
    command(manager, False)
    command(manager)
    manager._handle_configured_hotkey(kCGEventFlagsChanged, 56, kCGEventFlagMaskShift | kCGEventFlagMaskCommand)
    command(manager, False)
    assert manager.actions == []


def test_duplicate_modifier_events_trigger_only_once_on_real_release(manager):
    command(manager)
    command(manager)
    command(manager, False)
    command(manager, False)
    assert manager.actions == ["toggle"]


def test_missing_normal_key_release_does_not_permanently_block_hotkey(manager):
    manager._handle_configured_hotkey(kCGEventKeyDown, 0, 0)
    with patch("Quartz.CGEventSourceKeyState", return_value=False):
        command(manager)
        command(manager, False)
    assert manager.actions == ["toggle"]
