#!/usr/bin/env python3
"""Build a separate, credential-free host for real Sparkle install/restart tests.

Does not launch applications or change the installed Veyqa app. The generated
loopback feed and test bundle identifier never enter public release artifacts.
"""
import argparse
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parents[1]
VENDOR = ROOT / "build/vendor/sparkle-2.10.0.noindex"


def feed(root: Path, build: int):
    folder = root / "archives" / str(build)
    subprocess.run([str(VENDOR / "bin/generate_appcast"), "--account", "com.wjcdev.veyqa.updates",
                    "--maximum-deltas", "0", "--embed-release-notes", "--download-url-prefix",
                    "http://127.0.0.1:8769/", "-o", str(folder / "appcast.xml"), str(folder)], check=True)
    shutil.copy2(folder / "appcast.xml", root / "server/appcast.xml")
    subprocess.run([str(VENDOR / "bin/sign_update"), "--account", "com.wjcdev.veyqa.updates",
                    "--verify", str(root / "server/appcast.xml")], check=True)


def prepare():
    root = Path(tempfile.mkdtemp(prefix="updater-test-", suffix=".noindex", dir=ROOT / "outputs"))
    (root / "server").mkdir()
    (root / "installed").mkdir()
    config = json.loads((ROOT / "packaging/sparkle.json").read_text())
    for build in (1, 2, 3):
        folder = root / "archives" / str(build)
        app = folder / "UpdateTestHost.app"
        (app / "Contents/MacOS").mkdir(parents=True)
        (app / "Contents/Frameworks").mkdir()
        shutil.copy2(ROOT / "build/UpdateTestHost", app / "Contents/MacOS/UpdateTestHost")
        shutil.copy2(ROOT / "build/VeyqaUpdates.dylib", app / "Contents/Frameworks/VeyqaUpdates.dylib")
        subprocess.run(["ditto", str(VENDOR / "Sparkle.framework"), str(app / "Contents/Frameworks/Sparkle.framework")], check=True)
        info = dict(CFBundleName="Veyqa Update Test", CFBundleIdentifier="com.wjcdev.veyqa.updater-test",
                    CFBundleExecutable="UpdateTestHost", CFBundlePackageType="APPL",
                    CFBundleVersion=str(build), CFBundleShortVersionString=f"0.0.{build}",
                    LSMinimumSystemVersion="13.0", SUFeedURL="http://127.0.0.1:8769/appcast.xml",
                    SUPublicEDKey=config["public_key"], SURequireSignedFeed=True,
                    SUVerifyUpdateBeforeExtraction=True, SUSignedFeedFailureExpirationInterval=0,
                    SUEnableAutomaticChecks=False, SUAutomaticallyUpdate=False, SUAllowsAutomaticUpdates=False,
                    NSAppTransportSecurity=dict(NSAllowsLocalNetworking=True))
        (app / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
        subprocess.run(["/bin/zsh", "-c",
                        'source "$1/packaging/signing.zsh"; veyqa_configure_signing; veyqa_sign_app "$2" "$1/packaging/voice_input.entitlements"',
                        "test", str(ROOT), str(app)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        archive = folder / f"UpdateTestHost-{build}.zip"
        subprocess.run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(app), str(archive)], check=True)
        (folder / f"UpdateTestHost-{build}.md").write_text(f"隔离更新测试：构建 {build}。不读取用户数据。\n")
        shutil.copy2(archive, root / "server" / archive.name)
        if build == 1:
            subprocess.run(["ditto", str(app), str(root / "installed/UpdateTestHost.app")], check=True)
    feed(root, 2)
    print(root)


def tamper(root: Path):
    """Correctly signed test feed advertising an intentionally corrupted ZIP."""
    ns = "{http://www.andymatuschak.org/xml-namespaces/sparkle}"
    tree = ET.parse(root / "archives/3/appcast.xml")
    item = tree.find("./channel/item")
    item.find(ns + "version").text = "4"
    item.find(ns + "shortVersionString").text = "0.0.4"
    item.find("enclosure").set("url", "http://127.0.0.1:8769/UpdateTestHost-4.zip")
    corrupted = bytearray((root / "server/UpdateTestHost-3.zip").read_bytes())
    corrupted[-1] ^= 1
    (root / "server/UpdateTestHost-4.zip").write_bytes(corrupted)
    tree.write(root / "server/appcast.xml", encoding="utf-8", xml_declaration=True)
    subprocess.run([str(VENDOR / "bin/sign_update"), "--account", "com.wjcdev.veyqa.updates",
                    str(root / "server/appcast.xml")], check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--next", type=Path)
    parser.add_argument("--tamper", type=Path)
    args = parser.parse_args()
    if args.tamper:
        tamper(args.tamper.resolve())
    elif args.next:
        feed(args.next.resolve(), 3)
    else:
        prepare()
