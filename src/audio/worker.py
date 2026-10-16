"""Private stdin/stdout audio worker. No credentials, network, GUI or app startup.

One worker handles at most one recording. Native device calls cannot strand the
UI or the next recording; the parent owns a bounded deadline and reaps this child.
"""
import base64
import json
import os
import queue
import sys
import threading


def run_worker():
    # Commands are inherited private pipes, never a listening TCP port.
    commands = queue.Queue()
    outgoing = queue.Queue(maxsize=512)
    stopped = threading.Event()

    def read_commands():
        for line in sys.stdin.buffer:
            try:
                commands.put(json.loads(line))
            except (ValueError, UnicodeError):
                break
        stopped.set()

    def send():
        while True:
            event = outgoing.get()
            try:
                sys.stdout.buffer.write(json.dumps(event).encode() + b"\n")
                sys.stdout.buffer.flush()
            except (BrokenPipeError, OSError):
                os._exit(0)
            if event["type"] == "stopped":
                return

    threading.Thread(target=read_commands, daemon=True).start()
    sender = threading.Thread(target=send, daemon=True)
    sender.start()
    # No device access until the parent explicitly starts a recording.
    try:
        command = commands.get(timeout=30)
        if command.get("type") != "start":
            return
        from .recorder import AudioRecorder, sd
        selector = AudioRecorder()
        for key in ("AUDIO_INPUT_DEVICE", "AUDIO_DEVICE_POLICY"):
            os.environ[key] = str(command.get(key, ""))
        device, info = selector._get_best_input_device()
        if info is None:
            outgoing.put({"type": "error", "reason": "没有可用麦克风，请检查系统声音输入设置"})
            return
        rate = selector._capture_sample_rate(device, info)
        overflow = threading.Event()

        def callback(data, _frames, _time, status):
            if status:
                overflow.set()
                return
            try:
                outgoing.put_nowait({"type": "audio", "data": base64.b64encode(data.tobytes()).decode()})
            except queue.Full:
                overflow.set()

        stream = sd.InputStream(channels=1, samplerate=rate, dtype="float32", callback=callback,
                                device=device, latency="low")
        # Metadata must precede audio; the parent waits for actual start success.
        outgoing.put({"type": "format", "rate": rate, "device": info["name"]})
        stream.start()
        outgoing.put({"type": "started"})
        while not stopped.is_set():
            if overflow.is_set():
                outgoing.put({"type": "error", "reason": "麦克风数据中断，请检查设备后重试"})
                break
            try:
                command = commands.get(timeout=0.1)
            except queue.Empty:
                continue
            if command.get("type") == "stop":
                break
        # These calls may hang. Only this disposable worker can be affected.
        stream.stop()
        stream.close()
    except Exception:
        outgoing.put({"type": "error", "reason": "麦克风启动失败，请检查设备和麦克风权限"})
    finally:
        outgoing.put({"type": "stopped"})
        sender.join(timeout=2)


if __name__ == "__main__":
    run_worker()
