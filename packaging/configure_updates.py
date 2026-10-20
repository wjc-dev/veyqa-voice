"""Inject only public update configuration before sealing the app bundle."""
import json
import plistlib
from pathlib import Path
import sys

config = json.loads((Path(__file__).parent / "sparkle.json").read_text())
path = Path(sys.argv[1]) / "Contents/Info.plist"
with path.open("rb") as handle:
    info = plistlib.load(handle)
info.update({
    "SUFeedURL": config["feed_url"],
    "SUPublicEDKey": config["public_key"],
    "SUEnableAutomaticChecks": True,
    "SUScheduledCheckInterval": 86400,
    "SUAutomaticallyUpdate": False,
    "SUAllowsAutomaticUpdates": False,
    "SUEnableSystemProfiling": False,
    "SUVerifyUpdateBeforeExtraction": True,
    "SURequireSignedFeed": True,
    "SUSignedFeedFailureExpirationInterval": 0,
})
with path.open("wb") as handle:
    plistlib.dump(info, handle)
