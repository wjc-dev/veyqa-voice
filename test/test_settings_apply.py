import threading
from types import SimpleNamespace
from unittest.mock import Mock, patch, create_autospec

from main import VoiceAssistant
from src.ui.floating_preview import FloatingPreviewWindow


def assistant():
    app = VoiceAssistant.__new__(VoiceAssistant)
    app._current_session = None
    app._session_lock = threading.RLock()
    app._microphone_test_active = False
    app._streaming_thread = None
    app.keyboard_manager = SimpleNamespace(
        _recording_lock=threading.RLock(), is_recording=False,
        reload_configuration=Mock(), _hotkey_backend="passive",
        voice_hotkey_label="右 Option", _transition_started_at=None,
    )
    app.permission_monitor = SimpleNamespace(hotkey_backend="passive")
    app.status_controller = Mock()
    app.diagnostics = Mock()
    return app


def test_save_applies_without_restart_and_acknowledges_revision(tmp_path):
    app = assistant()
    request = tmp_path / ".settings-apply-request"
    request.write_text("save-1")
    with patch("main.DATA_DIR", tmp_path), patch("main.load_dotenv"), \
            patch("main.QwenStreamingProcessor") as processor, \
            patch("main.sync_login_item") as login, patch("main.os.execv") as restart:
        processor.return_value.is_available.return_value = True
        login.return_value = SimpleNamespace(status="enabled", error="")
        app._apply_pending_settings()
    restart.assert_not_called()
    app.keyboard_manager.reload_configuration.assert_called_once()
    app.status_controller.apply_preferences.assert_called_once()
    assert not request.exists()
    assert app.diagnostics.update.call_args.kwargs["settings_status"] == "applied"
    assert app.diagnostics.update.call_args.kwargs["settings_revision"] == "save-1"


def test_save_during_dictation_is_deferred(tmp_path):
    app = assistant()
    app._current_session = object()
    request = tmp_path / ".settings-apply-request"
    request.write_text("save-2")
    with patch("main.DATA_DIR", tmp_path), patch("main.load_dotenv") as load:
        app._apply_pending_settings()
    load.assert_not_called()
    app.keyboard_manager.reload_configuration.assert_not_called()
    app.diagnostics.update.assert_called_once_with(settings_revision="save-2", settings_status="pending")
    assert request.exists()


def test_failed_save_is_acknowledged_and_not_retried_forever(tmp_path):
    app = assistant()
    request = tmp_path / ".settings-apply-request"
    request.write_text("save-3")
    with patch("main.DATA_DIR", tmp_path), patch("main.load_dotenv", side_effect=OSError), \
            patch("main.os.execv") as restart:
        app._apply_pending_settings()
        app._apply_pending_settings()
    restart.assert_not_called()
    app.diagnostics.update.assert_called_once_with(settings_revision="save-3", settings_status="error")
    assert not request.exists()


def test_unconfigured_hotkey_does_not_open_settings():
    app = assistant()
    app.engine_label = "千问"
    app.keyboard_manager = Mock()
    app.floating_preview = create_autospec(FloatingPreviewWindow, instance=True)
    app.status_controller = Mock()
    app._start_unconfigured()
    app.status_controller.open_settings.assert_not_called()
    assert "点击 Dock" in app.floating_preview.show_notice.call_args.args[0]


def test_hotkey_in_settings_does_not_start_microphone():
    app = assistant()
    app.processor = Mock()
    app.keyboard_manager = Mock()
    app.keyboard_manager.frontmost_is_settings.return_value = True
    app.keyboard_manager.transition_was_cancelled.return_value = False
    app.floating_preview = create_autospec(FloatingPreviewWindow, instance=True)
    app.audio_recorder = Mock()
    app.start_streaming()
    app.audio_recorder.start_streaming_recording.assert_not_called()
    assert "文本框" in app.floating_preview.show_notice.call_args.args[0]


def test_microphone_test_notice_uses_real_interface_and_resets_keyboard():
    app = assistant()
    app._microphone_test_active = True
    app.keyboard_manager = Mock()
    app.floating_preview = create_autospec(FloatingPreviewWindow, instance=True)
    app.start_streaming()
    app.keyboard_manager.reset_state.assert_called_once()


def test_rejected_start_notice_does_not_cover_a_new_session():
    app = assistant()
    app.engine_label = "千问"
    app.keyboard_manager = Mock()
    app.floating_preview = create_autospec(FloatingPreviewWindow, instance=True)
    app._start_unconfigured()
    relevant = app.floating_preview.show_notice.call_args.kwargs["is_relevant"]
    assert relevant()
    app._current_session = object()
    assert not relevant()


def test_bad_credentials_are_not_reported_as_applied(tmp_path):
    app = assistant()
    request = tmp_path / ".settings-apply-request"
    request.write_text("bad-key")
    with patch("main.DATA_DIR", tmp_path), patch("main.load_dotenv"), \
            patch("main.QwenStreamingProcessor") as processor:
        processor.return_value.is_available.return_value = False
        app._apply_pending_settings()
    assert app.diagnostics.update.call_args.kwargs["settings_status"] == "error"
    app.keyboard_manager.reload_configuration.assert_not_called()


def test_trial_editor_can_start_recording_inside_settings():
    app = assistant()
    app.processor = Mock()
    app.keyboard_manager = Mock()
    app.keyboard_manager.transition_was_cancelled.return_value = False
    app.keyboard_manager.frontmost_is_settings.return_value = True
    app.keyboard_manager.is_setup_trial_target.return_value = True
    app.audio_recorder = Mock()
    app.audio_recorder.start_streaming_recording.return_value = "test device unavailable"
    app.floating_preview = create_autospec(FloatingPreviewWindow, instance=True)
    app.start_streaming()
    app.audio_recorder.start_streaming_recording.assert_called_once()
    assert app._current_session is None


def test_notice_failure_still_resets_pressed_state():
    import pytest
    app = assistant()
    app.keyboard_manager = Mock()
    app.floating_preview = create_autospec(FloatingPreviewWindow, instance=True)
    app.floating_preview.show_notice.side_effect = RuntimeError("UI unavailable")
    with pytest.raises(RuntimeError):
        app._reject_start("message")
    app.keyboard_manager.reset_state.assert_called_once()


def test_first_permission_grant_rebinds_once_without_restarting(tmp_path):
    app = assistant()
    with patch("main.DATA_DIR", tmp_path):
        _check_first_permission_grant(app)


def _check_first_permission_grant(app):
    app.permission_monitor.latest_snapshot = {"accessibility": "missing", "input_monitoring": "missing"}
    app._refresh_permission_listener()
    app.keyboard_manager.reload_configuration.assert_not_called()
    app.permission_monitor.latest_snapshot = {"accessibility": "granted", "input_monitoring": "granted"}
    app._current_session = object()
    app._refresh_permission_listener()
    app.keyboard_manager.reload_configuration.assert_not_called()
    app._current_session = None
    with patch("main.os.execv") as restart:
        app._refresh_permission_listener()
        app._refresh_permission_listener()
    app.keyboard_manager.reload_configuration.assert_called_once_with(force_listener=True)
    restart.assert_not_called()


def test_registered_listener_recovers_without_input_monitoring(tmp_path):
    app = assistant()
    app._last_permission_state = ("missing", "missing")
    app.keyboard_manager._hotkey_backend = "registered"
    app.permission_monitor.latest_snapshot = {"accessibility": "granted", "input_monitoring": "missing"}
    with patch("main.DATA_DIR", tmp_path):
        app._refresh_permission_listener()
        app._refresh_permission_listener()
    app.keyboard_manager.reload_configuration.assert_called_once_with(force_listener=True)


def test_manual_listener_reconnect_is_deferred_and_consumed_once(tmp_path):
    app = assistant()
    app._last_permission_state = ("granted", "granted")
    app.permission_monitor.latest_snapshot = {"accessibility": "granted", "input_monitoring": "granted"}
    request = tmp_path / ".listener-reconnect-request"
    request.touch()
    with patch("main.DATA_DIR", tmp_path):
        app.keyboard_manager.is_recording = True
        app._refresh_permission_listener()
        assert request.exists()
        app.keyboard_manager.reload_configuration.assert_not_called()
        app.keyboard_manager.is_recording = False
        app._refresh_permission_listener()
        app._refresh_permission_listener()
    assert not request.exists()
    app.keyboard_manager.reload_configuration.assert_called_once_with(force_listener=True)
