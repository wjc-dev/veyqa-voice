"""Private-build payload encoding, NOT protection against reverse engineering.

The seed ships with the payload. This only avoids accidental plaintext discovery
and keeps secrets out of source control; recipients can recover the credential.
"""
from dataclasses import dataclass
import hashlib
import json
import re
import secrets

# Legacy envelope marker, not the runtime model. Keep existing credentials valid;
# the current sponsored model comes from config/asr-models.json.
MODEL = "qwen-audio-3.0-asr-flash-streaming"
MAGIC = b"VEYQA-PRIVATE-1\x00"
FILENAME = "private-trial.dat"


@dataclass(frozen=True, repr=False)
class TrialCredential:
    key: str
    host: str


def validate(key, host):
    if (not isinstance(key, str) or not 20 <= len(key) <= 1024
            or not re.fullmatch(r"sk-[A-Za-z0-9._-]+", key)
            or not isinstance(host, str)
            or not re.fullmatch(r"[a-z0-9-]+\.cn-beijing\.maas\.aliyuncs\.com", host)):
        raise ValueError("Invalid private trial credential configuration")
    return TrialCredential(key, host)


def _mask(data, seed):
    stream = b"".join(hashlib.sha256(seed + i.to_bytes(4, "big")).digest()
                      for i in range((len(data) + 31) // 32))
    return bytes(a ^ b for a, b in zip(data, stream))


def encode(key, host):
    credential = validate(key, host)
    data = json.dumps({"key": credential.key, "host": credential.host, "model": MODEL}).encode()
    seed = secrets.token_bytes(32)
    return MAGIC + seed + hashlib.sha256(data).digest() + _mask(data, seed)


def decode(payload):
    try:
        if not isinstance(payload, bytes) or not len(MAGIC) + 64 < len(payload) <= 4096:
            raise ValueError()
        if not payload.startswith(MAGIC):
            raise ValueError()
        seed = payload[len(MAGIC):len(MAGIC) + 32]
        digest = payload[len(MAGIC) + 32:len(MAGIC) + 64]
        data = _mask(payload[len(MAGIC) + 64:], seed)
        if not secrets.compare_digest(hashlib.sha256(data).digest(), digest):
            raise ValueError()
        value = json.loads(data)
        if value.get("model") != MODEL:
            raise ValueError()
        return validate(value.get("key"), value.get("host"))
    except (ValueError, TypeError, AttributeError, UnicodeError):
        raise ValueError("Private trial payload is missing or damaged") from None
