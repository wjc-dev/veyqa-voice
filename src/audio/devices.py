"""Device selection shared by the app and lean worker, without native imports."""

PREFERRED_DEVICE_KEYWORDS = (
    "dji mic", "wireless mic", "external microphone", "accentum",
    "macbook pro microphone", "macbook air microphone", "airpods", "麦克风",
)


def select_input_device(sd, configured="", policy="system"):
    try:
        devices = sd.query_devices()
        inputs = [(i, device) for i, device in enumerate(devices)
                  if device["max_input_channels"] > 0]
        configured = configured.strip().lower()
        if configured:
            for index, device in inputs:
                if configured in device["name"].lower():
                    return index, device
        try:
            default = int(sd.default.device[0])
            system = next((item for item in inputs if item[0] == default), (None, None))
        except Exception:
            system = (None, None)
        if policy.strip().lower() != "preferred" and system[0] is not None:
            return system
        for keyword in PREFERRED_DEVICE_KEYWORDS:
            for index, device in inputs:
                if keyword in device["name"].lower():
                    return index, device
        return system
    except Exception:
        return None, None


def capture_sample_rate(sd, device_idx, device, preferred=16000):
    try:
        sd.check_input_settings(device=device_idx, channels=1, samplerate=preferred)
        return preferred
    except Exception:
        return int(device["default_samplerate"])
