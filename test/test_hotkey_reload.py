import os
import threading
from unittest.mock import Mock, patch

import pytest

from src.keyboard.listener import KeyboardManager, _PassiveTapListener


def manager():
    result = KeyboardManager.__new__(KeyboardManager)
    result.voice_hotkey = "right_option"
    result.voice_hotkey_label = "右 Option"
    result._hotkey_backend = "passive"
    result._listener = Mock()
    result._recording_lock = threading.RLock()
    return result


def test_passive_key_change_reuses_listener():
    result = manager()
    original = result._listener
    with patch.dict(os.environ, {"VOICE_HOTKEY": "right_command", "FN_HOTKEY_MODE": "toggle",
                                 "GLOBAL_HOTKEY_BACKEND": "passive"}), \
            patch.object(result, "start_listening") as start:
        result.reload_configuration()
    original.stop.assert_not_called()
    start.assert_not_called()
    assert result._listener is original
    assert result.voice_hotkey == "right_command"
    assert result._hotkey_mode == "toggle"


def test_registration_failure_does_not_report_success():
    result = manager()
    with patch.dict(os.environ, {"VOICE_HOTKEY": "keycode:49;mods:control+shift", "GLOBAL_HOTKEY_BACKEND": "registered"}), \
            patch.object(result, "start_listening", return_value=False), \
            pytest.raises(RuntimeError, match="注册失败"):
        result.reload_configuration()


def test_obsolete_listener_start_is_ignored():
    result = manager()
    result._listener_generation = 2
    with patch("src.keyboard.listener._PassiveTapListener") as listener:
        result.start_listening(1)
    listener.assert_not_called()


def test_stopped_tap_cannot_be_revived_by_delayed_start():
    listener = _PassiveTapListener(Mock())
    listener.stop()
    with listener:
        listener.join()
        assert not listener.running
