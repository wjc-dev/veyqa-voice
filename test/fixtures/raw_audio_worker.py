"""Run the real worker against a deterministic raw PortAudio substitute."""
import struct
import sys
from types import SimpleNamespace


class Stream:
    def __init__(self, **kwargs):
        assert kwargs["dtype"] == "float32"
        assert kwargs["channels"] == 1
        self.callback = kwargs["callback"]

    def start(self):
        self.callback(memoryview(struct.pack("<2f", 0.25, -0.5)), 2, None, None)

    def stop(self):
        pass

    def close(self):
        pass


sys.modules["sounddevice"] = SimpleNamespace(
    query_devices=lambda: [dict(name="test microphone", max_input_channels=1, default_samplerate=16000)],
    default=SimpleNamespace(device=(0, 0)),
    check_input_settings=lambda **kwargs: None,
    RawInputStream=Stream,
)

from src.audio.worker import run_worker
run_worker()
