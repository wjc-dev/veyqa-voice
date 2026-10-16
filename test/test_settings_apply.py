import threading
from types import SimpleNamespace
from unittest.mock import Mock, patch

from main import VoiceAssistant


def assistant():
    app = VoiceAssistant.__new__(VoiceAssistant)
    app._current_session = None
    app._microphone_test_active = False
    app._streaming_thread = None
    app.keyboard_manager = SimpleNamespace(
        _recording_lock=threading.RLock(), is_recording=False,
        reload_configuration=Mock(), _hotkey_backend="passive",
        voice_hotkey_label="右 Option", _transition_started_at=None,
    )
    app.permission_monitor = SimpleNamespace(hotkey_backend="passive")
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
    app.floating_preview = Mock()
    app.status_controller = Mock()
    app._start_unconfigured()
    app.status_controller.open_settings.assert_not_called()
    assert "点击 Dock" in app.floating_preview.show_notice.call_args.args[0]


def test_hotkey_in_settings_does_not_start_microphone():
    app = assistant()
    app.processor = Mock()
    app.keyboard_manager = Mock()
    app.keyboard_manager.frontmost_is_settings.return_value = True
    app.floating_preview = Mock()
    app.audio_recorder = Mock()
    app.start_streaming()
    app.audio_recorder.start_streaming_recording.assert_not_called()
    assert "文本框" in app.floating_preview.show_notice.call_args.args[0]
