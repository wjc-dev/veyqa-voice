"""Generate and verify the Sparkle appcast for a public Rust build.

Like the Python public release, the built-in service uses an offline-provisioned
credential embedded as `private-trial.dat`; the app never talks to the
anonymous-credential service. The archive must therefore contain that payload,
enable the trial gate, and match every update key.
"""
import argparse
import json
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

PACKAGING_ROOT = Path(
    os.environ.get("VEYQA_PACKAGING_ROOT", Path.home() / "Documents" / "voxtype")
).resolve()
sys.path.insert(0, str(PACKAGING_ROOT / "packaging"))

from prepare_sparkle import prepare  # noqa: E402

FEED_LINK = "https://wjc-dev.github.io/veyqa-voice/"
APPCAST_NS = "{http://www.andymatuschak.org/xml-namespaces/sparkle}"


def inspect(archive: Path, config: dict) -> dict:
    with zipfile.ZipFile(archive) as source:
        names = source.namelist()
        if "Veyqa Voice.app/Contents/Info.plist" not in names:
            raise ValueError("Archive does not contain Veyqa Voice.app at its root")
        info = plistlib.loads(source.read("Veyqa Voice.app/Contents/Info.plist"))
        features_name = "Veyqa Voice.app/Contents/Resources/release-features.json"
        if features_name not in names:
            raise ValueError("Archive has no release feature flags")
        features = json.loads(source.read(features_name))
        if not any(Path(name).name == "private-trial.dat" for name in names):
            raise ValueError("Public build is missing its built-in service credential")
    if features.get("builtin_speech") is not True or features.get("private_trial") is not True:
        raise ValueError("Archive does not enable the built-in trial service")
    if info.get("CFBundleIdentifier") != "com.wjcdev.veyqa":
        raise ValueError("Archive is not the production bundle identifier")
    if info.get("VeyqaDistribution") != "public-promotion":
        raise ValueError("Archive is not marked as a public promotion build")
    if info.get("VeyqaSigningProfile") != "local":
        raise ValueError("Refusing to publish an ad-hoc update archive")
    if info.get("SUFeedURL") != config["feed_url"]:
        raise ValueError("Archive points at a different update feed")
    if info.get("SUPublicEDKey") != config["public_key"] or not info.get("SURequireSignedFeed"):
        raise ValueError("Archive uses a different signing key or an unsigned feed")
    if not info.get("SUEnableAutomaticChecks") or not info.get("SUVerifyUpdateBeforeExtraction"):
        raise ValueError("Archive update configuration is incomplete")
    if info.get("CFBundleShortVersionString") is None or info.get("CFBundleVersion") is None:
        raise ValueError("Archive has no version metadata")
    return info


def generate(archive: Path, output: Path, notes: Path, prefix: str | None = None) -> dict:
    config = json.loads((PACKAGING_ROOT / "packaging" / "sparkle.json").read_text())
    info = inspect(archive, config)
    version = info["CFBundleShortVersionString"]
    prefix = prefix or f"https://github.com/wjc-dev/veyqa-voice/releases/download/v{version}/"
    if not prefix.startswith("https://github.com/wjc-dev/veyqa-voice/releases/download/v"):
        raise ValueError("Only official HTTPS release URLs may be published")
    tools = prepare() / "bin"
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix="appcast-", suffix=".noindex", dir=str(output.parent)
    ) as directory:
        staging = Path(directory)
        shutil.copy2(archive, staging / archive.name)
        shutil.copy2(notes, staging / (archive.stem + ".md"))
        generated = staging / "appcast.xml"
        subprocess.run(
            [
                str(tools / "generate_appcast"),
                "--account", config["keychain_account"],
                "--maximum-deltas", "0",
                "--embed-release-notes",
                "--download-url-prefix", prefix,
                "--link", FEED_LINK,
                "-o", str(generated),
                str(staging),
            ],
            check=True,
        )
        subprocess.run(
            [str(tools / "sign_update"), "--account", config["keychain_account"],
             "--verify", str(generated)],
            check=True,
        )
        item = ET.parse(generated).find("./channel/item")
        enclosure = item.find("enclosure")
        if (
            item.findtext(APPCAST_NS + "version") != info["CFBundleVersion"]
            or int(enclosure.get("length", "0")) != archive.stat().st_size
            or enclosure.get("url") != prefix + archive.name
            or not enclosure.get(APPCAST_NS + "edSignature")
        ):
            raise ValueError("Generated appcast does not match the archive")
        shutil.copy2(generated, output)
    print(f"Verified update feed: {version} ({info['CFBundleVersion']}) -> {output}")
    return info


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--notes", type=Path, required=True)
    args = parser.parse_args()
    generate(args.archive.resolve(), args.output.resolve(), args.notes.resolve())
