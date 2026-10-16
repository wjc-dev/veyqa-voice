"""Keep all PortAudio calls out of the resident app; preserve its audio API."""
import atexit
import base64
import json
import os
import subprocess
import sys
import threading
import time
import weakref
from pathlib import Path

import numpy as np
from .recorder import AudioRecorder

_recorders = weakref.WeakSet()


@atexit.register
def _close_recorders():
    for recorder in list(_recorders):
        recorder.close()


class IsolatedAudioRecorder(AudioRecorder):
    start_timeout = 4.0
    stop_timeout = 1.5

    def __init__(self, command=None):
        super().__init__()
        self._command = command
        self._process = None
        self._generation = 0
        self._ready = threading.Event()
        self._ended = threading.Event()
        self._error = ""
        self._lifecycle_lock = threading.RLock()
        # Do not retain every temporary microphone-test recorder until exit.
        _recorders.add(self)

    @property
    def capture_error(self):
        return self._error

    def _spawn(self):
        command = self._command or ([sys.executable, "--audio-worker"] if getattr(sys, "frozen", False)
                                    else [sys.executable, "-m", "src.audio.worker"])
        # Never forward API keys or the settings helper's runtime environment.
        environment = {k: v for k, v in os.environ.items()
                       if k in {"HOME", "PATH", "TMPDIR", "LANG", "LC_ALL", "VOICE_INPUT_DATA_DIR"}}
        return subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, env=environment,
                                cwd=str(Path(__file__).resolve().parents[2]), bufsize=0)

    def _send(self, message):
        process = self._process
        if process is None or process.poll() is not None:
            raise RuntimeError("录音工作进程已退出")
        process.stdin.write(json.dumps(message).encode() + b"\n")
        process.stdin.flush()

    def _read(self, process, generation, ready, ended):
        try:
            for line in process.stdout:
                event = json.loads(line)
                with self._lifecycle_lock:
                    if generation != self._generation:
                        return
                    kind = event.get("type")
                    if kind == "format":
                        self.sample_rate = int(event["rate"])
                        self.current_device = str(event["device"])
                    elif kind == "audio" and self.recording:
                        samples = np.frombuffer(base64.b64decode(event["data"], validate=True), dtype=np.float32).copy()
                        self._capture_audio_chunk(samples, stream_to_queue=True)
                    elif kind == "started":
                        ready.set()
                    elif kind == "error":
                        self._error = str(event["reason"])
                        ready.set()
                    elif kind == "stopped":
                        ended.set()
                        return
        except (OSError, ValueError, KeyError):
            pass
        finally:
            with self._lifecycle_lock:
                if generation == self._generation:
                    self.recording = False
                    if not ended.is_set():
                        self._error = self._error or "录音工作进程中断，请重试"
                        ready.set()
                ended.set()

    def start_streaming_recording(self):
        self.close()
        with self._lifecycle_lock:
            self._generation += 1
            generation = self._generation
            self._ready, self._ended = threading.Event(), threading.Event()
            self._error = ""
            self._start_capture_session(clear_queue=True)
            try:
                self._process = process = self._spawn()
                threading.Thread(target=self._read, args=(process, generation, self._ready, self._ended),
                                 daemon=True, name="audio-worker-reader").start()
                self._send({"type": "start", "AUDIO_INPUT_DEVICE": os.getenv("AUDIO_INPUT_DEVICE", ""),
                            "AUDIO_DEVICE_POLICY": os.getenv("AUDIO_DEVICE_POLICY", "system")})
            except (OSError, RuntimeError):
                self.close()
                return "录音工作进程无法启动，请检查系统是否拦截应用"
            ready = self._ready
        if not ready.wait(self.start_timeout):
            self.close()
            return "麦克风准备超时，本次录音已取消；请检查设备后重试"
        with self._lifecycle_lock:
            if generation != self._generation:
                return "本次录音已取消"
            if self._error:
                error = self._error
                self.close()
                return error
            self.auto_stop_timer = threading.Timer(self.max_record_duration, self._auto_stop_recording)
            self.auto_stop_timer.daemon = True
            self.auto_stop_timer.start()
        return None

    def stop_streaming_recording(self, abort=False):
        if abort:
            self.reset_streaming_state()
            return None
        if self.recording and self.post_roll_ms:
            time.sleep(self.post_roll_ms / 1000)
        try:
            self._send({"type": "stop"})
        except (OSError, RuntimeError):
            pass
        ended = self._ended.wait(self.stop_timeout)
        with self._lifecycle_lock:
            self.recording = False
            self._cancel_auto_stop_timer()
            buffer = self._build_audio_buffer()
            self.close()
        if not ended:
            # All audio delivered before the stall remains in audio_queue.
            from ..utils.logger import logger
            logger.warning("麦克风停止超时，已结束录音工作进程；主程序保持运行")
        return buffer

    def close(self):
        with self._lifecycle_lock:
            self.recording = False
            self._cancel_auto_stop_timer()
            self._generation += 1
            self._ready.set()
            process, self._process = self._process, None
            if process is not None:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=0.3)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        try:
                            process.wait(timeout=0.5)
                        except subprocess.TimeoutExpired:
                            # Reap asynchronously; never strand a hotkey transition.
                            threading.Thread(target=process.wait, daemon=True).start()
                for pipe in (process.stdin, process.stdout):
                    if pipe:
                        pipe.close()

    def reset_streaming_state(self, reason="", drain_queue=True):
        self.recording = False
        self._cancel_auto_stop_timer()
        self.close()
        self.record_start_time = None
        self._reset_recorded_audio()
        if drain_queue:
            self._drain_audio_queue()
