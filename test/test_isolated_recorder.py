import os
import sys
import threading
import time
from pathlib import Path
from unittest.mock import patch

from src.audio.isolated_recorder import IsolatedAudioRecorder

FIXTURE = Path(__file__).parent / "fixtures/audio_worker.py"


def recorder(mode):
    result = IsolatedAudioRecorder([sys.executable, str(FIXTURE), mode])
    result.start_timeout = 0.4
    result.stop_timeout = 0.2
    result.post_roll_ms = 0
    return result


def test_start_timeout_reaps_only_worker_and_next_session_succeeds():
    capture = recorder("start-stall")
    pid = os.getpid()
    assert "超时" in capture.start_streaming_recording()
    assert capture._process is None
    assert not capture.recording
    capture._command[-1] = "ok"
    assert capture.start_streaming_recording() is None
    assert capture.stop_streaming_recording() is not None
    assert os.getpid() == pid
    assert capture._process is None


def test_stop_timeout_keeps_delivered_audio_and_is_bounded():
    capture = recorder("stop-stall")
    assert capture.start_streaming_recording() is None
    started = time.monotonic()
    assert capture.stop_streaming_recording() is not None
    assert time.monotonic() - started < 1.5
    assert capture._process is None
    assert not capture.audio_queue.empty()


def test_cancel_wakes_pending_start_and_cleans_audio():
    capture = recorder("start-stall")
    result = []
    thread = threading.Thread(target=lambda: result.append(capture.start_streaming_recording()))
    thread.start()
    deadline = time.monotonic() + 1
    while capture._process is None and time.monotonic() < deadline:
        time.sleep(0.005)
    capture.reset_streaming_state()
    thread.join(1)
    assert not thread.is_alive()
    assert result and result[0] is not None
    assert capture._process is None
    assert not capture.recording
    assert capture.audio_queue.empty()


def test_worker_environment_does_not_receive_api_credentials():
    capture = recorder("ok")
    with patch.dict(os.environ, {"QWEN_API_KEY": "test-only", "DOUBAO_ACCESS_KEY": "test-only"}), \
            patch("src.audio.isolated_recorder.subprocess.Popen") as spawn:
        capture._spawn()
    environment = spawn.call_args.kwargs["env"]
    assert "QWEN_API_KEY" not in environment
    assert "DOUBAO_ACCESS_KEY" not in environment
