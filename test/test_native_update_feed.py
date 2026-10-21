"""Exercise the real Objective-C retry policy without starting AppKit UI."""
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def test_native_update_feed_retry_contract(tmp_path):
    vendor = ROOT / "build/vendor/sparkle-2.10.0.noindex"
    library = tmp_path / "feed-tests.dylib"
    subprocess.run([
        "xcrun", "clang", "-dynamiclib", "-fobjc-arc", "-Werror", "-Wno-nullability-completeness",
        "-framework", "AppKit", "-framework", "Sparkle", "-F", str(vendor),
        "-Wl,-rpath," + str(vendor), str(ROOT / "test/update_feed_harness.m"),
        "-o", str(library),
    ], check=True, capture_output=True, text=True, timeout=60)
    # A library loaded in a test subprocess avoids registering another .app.
    result = subprocess.run([
        sys.executable, "-c",
        "import ctypes,sys; result=ctypes.CDLL(sys.argv[1]).VeyqaRunFeedTests(sys.argv[2].encode()); "
        "assert result == 0, f'Native assertion failed at line {result}'",
        str(library), str(tmp_path / "state.json"),
    ], capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, result.stderr
