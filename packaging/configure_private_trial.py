"""Create/inject an offline trial payload. Never print or accept a key in argv."""
import argparse
import json
import os
from pathlib import Path
import plistlib
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from src.transcription.private_trial_payload import FILENAME, decode, encode


def inject(app, payload_path):
    payload = payload_path.read_bytes()
    decode(payload)
    info_path = app / "Contents/Info.plist"
    info = plistlib.loads(info_path.read_bytes())
    info["VeyqaDistribution"] = "private-trial"
    info["SUEnableAutomaticChecks"] = False
    info.pop("SUFeedURL", None)
    roots = [app / "Contents/Resources",
             app / "Contents/Helpers/VeyqaSettings.app/Contents/Resources"]
    flags = json.dumps({"builtin_speech": True, "private_trial": True}) + "\n"
    for root in roots:
        if not root.is_dir():
            raise ValueError("Expected app resources missing")
        (root / "release-features.json").write_text(flags)
    # Only the main process holds the credential; the settings helper has flags.
    destination = roots[0] / FILENAME
    destination.write_bytes(payload)
    # PKG installs are root-owned: runtime resources must remain user-readable.
    # Secrecy comes only from private distribution, not this file's permissions.
    destination.chmod(0o644)
    # PyInstaller's resource root is Frameworks; data files normally have these
    # cross-directory links created by PyInstaller before our private injection.
    link = app / "Contents/Frameworks" / FILENAME
    if link.exists() or link.is_symlink():
        raise ValueError("Private payload already injected")
    link.symlink_to("../Resources/" + FILENAME)
    info_path.write_bytes(plistlib.dumps(info))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    create = sub.add_parser("encode")
    create.add_argument("--host", required=True)
    create.add_argument("--output", type=Path, required=True)
    install = sub.add_parser("inject")
    install.add_argument("app", type=Path)
    install.add_argument("payload", type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        if args.action == "encode":
            key = sys.stdin.read(2048).strip()
            payload = encode(key, args.host)
            with args.output.open("xb") as handle:
                handle.write(payload)
            print("Private trial payload prepared; no credential printed.")
        else:
            inject(args.app, args.payload)
            print("Private trial resources injected; public updates disabled.")
    except Exception:
        print("Private trial preparation failed; check inputs and output paths.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
