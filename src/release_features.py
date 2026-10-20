"""Packaged release gates, shared with the native settings helper."""
import json
from functools import lru_cache

from .runtime_paths import RESOURCE_ROOT


@lru_cache(maxsize=1)
def _flags():
    paths = (RESOURCE_ROOT / "release-features.json",
             RESOURCE_ROOT / "config" / "release-features.json")
    for path in paths:
        if path.exists():
            try:
                flags = json.loads(path.read_text())
                return flags if isinstance(flags, dict) else {}
            except (OSError, ValueError, AttributeError):
                return {}
    return {}


def builtin_speech_enabled():
    return _flags().get("builtin_speech") is True


def private_trial_enabled():
    return builtin_speech_enabled() and _flags().get("private_trial") is True
