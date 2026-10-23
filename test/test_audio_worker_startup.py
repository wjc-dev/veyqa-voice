"""Worker import boundary and shared device selection; no live audio."""
import json
import subprocess
import sys
import base64
import struct
from types import SimpleNamespace
from unittest.mock import Mock


def test_importing_worker_does_not_load_heavy_recorder_or_device_driver():
    result = subprocess.run([sys.executable, "-c", "import src.audio.worker,sys,json; print(json.dumps(sorted(set(sys.modules)&{'numpy','soundfile','sounddevice','src.audio.recorder','src.utils.logger'})))"], capture_output=True, text=True, check=True)
    assert json.loads(result.stdout) == []


def test_shared_device_selection_preserves_system_and_explicit_choice():
    from src.audio.devices import select_input_device, capture_sample_rate
    devices = [dict(name="MacBook Pro麦克风", max_input_channels=1, default_samplerate=48000),
               dict(name="AirPods", max_input_channels=1, default_samplerate=24000)]
    sd = SimpleNamespace(query_devices=lambda: devices, default=SimpleNamespace(device=(0, 0)),
                         check_input_settings=Mock())
    assert select_input_device(sd, "", "system") == (0, devices[0])
    assert select_input_device(sd, "airpods", "system") == (1, devices[1])
    assert select_input_device(sd, "", "preferred") == (1, devices[1])
    assert capture_sample_rate(sd, 0, devices[0]) == 16000
    sd.check_input_settings.side_effect = ValueError("unsupported")
    assert capture_sample_rate(sd, 1, devices[1]) == 24000


def test_shared_device_selection_handles_missing_device():
    from src.audio.devices import select_input_device
    sd = SimpleNamespace(query_devices=lambda: [], default=SimpleNamespace(device=(-1, -1)))
    assert select_input_device(sd, "", "system") == (None, None)


def test_raw_worker_preserves_float32_bytes_and_protocol_order():
    result = subprocess.run([sys.executable, "-m", "test.fixtures.raw_audio_worker"],
                            input='{"type":"start"}\n{"type":"stop"}\n',
                            capture_output=True, text=True, timeout=5, check=True)
    events = [json.loads(line) for line in result.stdout.splitlines()]
    assert [event["type"] for event in events] == ["format", "audio", "started", "stopped"]
    assert struct.unpack("<2f", base64.b64decode(events[1]["data"])) == (0.25, -0.5)
