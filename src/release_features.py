"""Packaged release gates, shared with the native settings helper."""
import json
from functools import lru_cache

from .runtime_paths import RESOURCE_ROOT


@lru_cache(maxsize=1)
def builtin_speech_enabled():
    paths = (RESOURCE_ROOT / "release-features.json",
             RESOURCE_ROOT / "config" / "release-features.json")
    for path in paths:
        if path.exists():
            try:
                return json.loads(path.read_text()).get("builtin_speech") is True
            except (OSError, ValueError, AttributeError):
                return False
    return False
