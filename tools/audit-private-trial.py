"""Audit a private ZIP without printing its credential."""
import argparse
import json
from pathlib import Path
import plistlib
import sys
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from src.transcription.private_trial_payload import FILENAME, decode


def audit(archive):
    prefix = "Veyqa Voice.app/Contents/"
    with zipfile.ZipFile(archive) as package:
        info = plistlib.loads(package.read(prefix + "Info.plist"))
        if info.get("VeyqaDistribution") != "private-trial" or info.get("SUFeedURL"):
            raise ValueError("Distribution/update channel mismatch")
        if info.get("SUEnableAutomaticChecks") is not False:
            raise ValueError("Public update checks must be disabled")
        secret = decode(package.read(prefix + "Resources/" + FILENAME))
        for name in package.namelist():
            if secret.key.encode() in package.read(name):
                raise ValueError("Plaintext credential detected in package")
        for resource in ("Resources/", "Helpers/VeyqaSettings.app/Contents/Resources/"):
            flags = json.loads(package.read(prefix + resource + "release-features.json"))
            if flags != {"builtin_speech": True, "private_trial": True}:
                raise ValueError("Main/helper release gates disagree")
        helper_payload = prefix + "Helpers/VeyqaSettings.app/Contents/Resources/" + FILENAME
        if helper_payload in package.namelist():
            raise ValueError("Settings helper must not contain the credential")
        if package.read(prefix + "Frameworks/" + FILENAME).decode() != "../Resources/" + FILENAME:
            raise ValueError("Frozen runtime resource link missing")
        permissions = package.getinfo(prefix + "Resources/" + FILENAME).external_attr >> 16
        if permissions & 0o444 != 0o444:
            raise ValueError("Credential resource unreadable after PKG install")
        return {"passed": True, "version": info["CFBundleShortVersionString"],
                "build": info["CFBundleVersion"], "plaintext_key_found": False,
                "settings_helper_contains_key": False, "public_updates_enabled": False,
                "main_and_helper_private_gates_match": True}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    try:
        result = audit(args.archive)
    except Exception as error:
        result = {"passed": False, "error_type": type(error).__name__}
    args.report.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
    raise SystemExit(0 if result["passed"] else 1)
