"""Fetch the pinned upstream Sparkle distribution, never keys or user data."""
import hashlib
import json
from pathlib import Path
import tarfile
import urllib.request


ROOT = Path(__file__).resolve().parents[1]


def prepare():
    config = json.loads((ROOT / "packaging/sparkle.json").read_text())
    cache = ROOT / "build/vendor"
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / f"Sparkle-{config['version']}.tar.xz"
    destination = cache / f"sparkle-{config['version']}.noindex"
    if not archive.exists():
        import truststore
        truststore.inject_into_ssl()
        with urllib.request.urlopen(config["url"], timeout=120) as response:
            data = response.read(100 * 1024 * 1024 + 1)
        if len(data) > 100 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != config["sha256"]:
            raise ValueError("Sparkle download checksum mismatch")
        archive.write_bytes(data)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != config["sha256"]:
        raise ValueError("Cached Sparkle checksum mismatch")
    if not (destination / "Sparkle.framework").is_dir():
        destination.mkdir(parents=True, exist_ok=True)
        with tarfile.open(archive) as source:
            source.extractall(destination, filter="data")
    print(destination)
    return destination


if __name__ == "__main__":
    prepare()
