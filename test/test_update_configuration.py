"""Release feed transport must not weaken Sparkle's trust boundary."""
import json
import plistlib
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def test_public_feed_uses_official_repository_and_keeps_verification(tmp_path):
    app = tmp_path / "Test.app"
    contents = app / "Contents"
    contents.mkdir(parents=True)
    path = contents / "Info.plist"
    path.write_bytes(plistlib.dumps({"CFBundleIdentifier": "com.wjcdev.veyqa"}))
    subprocess.run([sys.executable, str(ROOT / "packaging/configure_updates.py"),
                    str(app)], check=True)
    info = plistlib.loads(path.read_bytes())
    config = json.loads((ROOT / "packaging/sparkle.json").read_text())
    assert info["SUFeedURL"] == (
        "https://raw.githubusercontent.com/wjc-dev/veyqa-voice/main/website/appcast.xml"
    )
    assert info["SUPublicEDKey"] == config["public_key"]
    assert info["SURequireSignedFeed"] is True
    assert info["SUVerifyUpdateBeforeExtraction"] is True
    assert info["SUSignedFeedFailureExpirationInterval"] == 0
    assert info["SUEnableAutomaticChecks"] is True
    assert info["SUScheduledCheckInterval"] == 86400
    assert info["SUAutomaticallyUpdate"] is False
    assert info["SUAllowsAutomaticUpdates"] is False
    assert info["SUEnableSystemProfiling"] is False
    assert info["CFBundleIdentifier"] == "com.wjcdev.veyqa"
