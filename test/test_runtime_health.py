import json
import queue
import tempfile
import threading
import time
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from main import VoiceAssistant
from src.keyboard.inputState import InputState
from src.keyboard.listener import KeyboardManager
from src.runtime_health import RuntimeHealth, allow_automatic_restart


def test_healthy_idle_never_restarts(tmp_path):
    now = [0.0]
    recover = Mock()
    health = RuntimeHealth(lambda _: "", recover, path=tmp_path / "health.json", clock=lambda: now[0])
    for _ in range(100):
        now[0] += 2
        health.tick_main()
        health.poll()
    recover.assert_not_called()
    assert json.loads(health.path.read_text())["fault"] == ""


def test_fault_recovers_once_not_every_poll(tmp_path):
    recover = Mock()
    health = RuntimeHealth(lambda _: "录音设备操作超时", recover, path=tmp_path / "health.json")
    health.poll()
    health.poll()
    recover.assert_called_once_with("录音设备操作超时")


def test_unresponsive_main_loop_is_detected(tmp_path):
    now = [0.0]
    recover = Mock()
    health = RuntimeHealth(lambda _: "", recover, path=tmp_path / "health.json", clock=lambda: now[0])
    for _ in range(12):
        now[0] += 2
        health.poll()
    recover.assert_called_once_with("应用事件循环无响应")


def test_sleep_gap_does_not_immediately_restart(tmp_path):
    now = [0.0]
    recover = Mock()
    health = RuntimeHealth(lambda _: "", recover, path=tmp_path / "health.json", clock=lambda: now[0])
    now[0] = 3600
    health.poll()
    recover.assert_not_called()


def test_heartbeat_writes_are_throttled_without_delaying_fault_detection(tmp_path):
    now = [0.0]
    reason = [""]
    recover = Mock()
    health = RuntimeHealth(lambda _: reason[0], recover, path=tmp_path / "health.json", clock=lambda: now[0])
    with patch.object(health.store, "update") as write:
        for tick in range(0, 10, 2):
            now[0] = tick
            health.tick_main()
            health.poll()
        assert write.call_count == 1
        now[0] = 10
        health.tick_main()
        health.poll()
        assert write.call_count == 2
        reason[0] = "录音设备操作超时"
        now[0] = 12
        health.poll()
        assert write.call_count == 3
        recover.assert_called_once_with(reason[0])


def test_heartbeat_is_atomic_but_does_not_force_disk_flush(tmp_path):
    health = RuntimeHealth(lambda _: "", Mock(), path=tmp_path / "health.json")
    with patch("src.diagnostics.os.fsync") as fsync:
        health.poll()
    fsync.assert_not_called()
    assert json.loads(health.path.read_text())["pid"] > 1


def test_restart_budget_survives_new_process_and_expires(tmp_path):
    path = tmp_path / "attempts.json"
    assert all(allow_automatic_restart(path, now=100 + i) for i in range(3))
    assert not allow_automatic_restart(path, now=110)
    assert allow_automatic_restart(path, now=500)
    assert path.stat().st_mode & 0o777 == 0o600


def test_cancel_not_queued_behind_blocking_device_start():
    entered, unblock, cancelled = threading.Event(), threading.Event(), threading.Event()
    manager = KeyboardManager.__new__(KeyboardManager)
    manager._state = InputState.IDLE
    manager.is_recording = False
    manager._recording_lock = threading.RLock()
    manager._transition_queue = queue.Queue()
    manager._transition_started_at = None
    manager.on_state_change = None
    manager.on_record_start = lambda: (entered.set(), unblock.wait(2))
    manager.on_record_cancel = cancelled.set
    worker = threading.Thread(target=manager._transition_worker, daemon=True)
    worker.start()
    try:
        manager.start_recording()
        assert entered.wait(1)
        manager.cancel_recording()
        assert cancelled.wait(0.5)
        assert manager._transition_queue.empty()
        assert manager._transition_started_at is not None
    finally:
        unblock.set()


def test_stalled_audio_detected_but_long_recording_allowed():
    app = VoiceAssistant.__new__(VoiceAssistant)
    app.keyboard_manager = SimpleNamespace(_transition_started_at=1, _listener=None)
    app._current_state = InputState.RECORDING
    app._state_since = 1
    assert app._health_fault(12) == "录音设备操作超时"
    app.keyboard_manager._transition_started_at = None
    app._current_state = InputState.STREAMING
    assert app._health_fault(400) == ""
    app._current_state = InputState.PROCESSING
    assert app._health_fault(400) == "识别结束超时"


def test_cancel_before_worker_starts_never_opens_microphone():
    manager = KeyboardManager.__new__(KeyboardManager)
    manager._state = InputState.IDLE
    manager.is_recording = False
    manager._recording_lock = threading.RLock()
    manager._transition_queue = queue.Queue()
    manager._transition_started_at = None
    manager.on_state_change = None
    manager.on_record_start = Mock()
    manager.on_record_cancel = Mock()
    manager.start_recording()
    manager.cancel_recording()
    worker = threading.Thread(target=manager._transition_worker, daemon=True)
    worker.start()
    manager._transition_queue.join()
    manager.on_record_start.assert_not_called()


def test_cancel_after_dequeue_before_session_does_not_start_audio():
    app = VoiceAssistant.__new__(VoiceAssistant)
    app.processor = SimpleNamespace(is_available=lambda: True)
    app._session_lock = threading.RLock()
    app.keyboard_manager = SimpleNamespace(transition_was_cancelled=lambda: True)
    app.audio_recorder = Mock()
    app.start_streaming()
    app.audio_recorder.start_streaming_recording.assert_not_called()


def test_auto_restart_keeps_settings_closed():
    with patch("main.sys.argv", ["VeyqaVoice", "--show-settings-after-restart"]), patch("main.os.execv") as execute:
        VoiceAssistant._restart_application(show_settings=False)
    assert execute.call_args.args[1] == [execute.call_args.args[0], "--background-login"]
