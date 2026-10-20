"""Generate and verify an appcast using Sparkle's own signed-feed tools."""
import argparse
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET
import zipfile

from prepare_sparkle import prepare, ROOT


def generate(archive: Path, output: Path, notes: Path, prefix: str | None = None):
    config = json.loads((ROOT / "packaging/sparkle.json").read_text())
    tools = prepare() / "bin"
    with zipfile.ZipFile(archive) as source:
        info = plistlib.loads(source.read("Veyqa Voice.app/Contents/Info.plist"))
    if info.get("SUPublicEDKey") != config["public_key"] or not info.get("SURequireSignedFeed"):
        raise ValueError("Update archive has a different signing key or unsigned feed configuration")
    if info.get("VeyqaSigningProfile") != "local":
        raise ValueError("Refusing to publish an ad-hoc update archive")
    version = info["CFBundleShortVersionString"]
    prefix = prefix or f"https://github.com/wjc-dev/voxtype/releases/download/v{version}/"
    if not prefix.startswith("https://github.com/wjc-dev/voxtype/releases/download/v"):
        raise ValueError("Only official HTTPS release URLs may be published")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="appcast-", suffix=".noindex", dir=ROOT / "outputs") as directory:
        staging = Path(directory)
        shutil.copy2(archive, staging / archive.name)
        shutil.copy2(notes, staging / (archive.stem + ".md"))
        generated = staging / "appcast.xml"
        subprocess.run([str(tools / "generate_appcast"), "--account", config["keychain_account"],
                        "--maximum-deltas", "0", "--embed-release-notes", "--download-url-prefix", prefix,
                        "--link", "https://wjc-dev.github.io/voxtype/", "-o", str(generated), str(staging)], check=True)
        subprocess.run([str(tools / "sign_update"), "--account", config["keychain_account"],
                        "--verify", str(generated)], check=True)
        ns = "{http://www.andymatuschak.org/xml-namespaces/sparkle}"
        item = ET.parse(generated).find("./channel/item")
        enclosure = item.find("enclosure")
        if (item.findtext(ns + "version") != info["CFBundleVersion"]
                or int(enclosure.get("length", "0")) != archive.stat().st_size
                or enclosure.get("url") != prefix + archive.name
                or not enclosure.get(ns + "edSignature")):
            raise ValueError("Generated appcast does not match archive")
        shutil.copy2(generated, output)
    print(f"Verified update feed: {version} ({info['CFBundleVersion']})")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--notes", type=Path, required=True)
    args = parser.parse_args()
    generate(args.archive.resolve(), args.output.resolve(), args.notes.resolve())
