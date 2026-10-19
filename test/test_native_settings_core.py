import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class NativeSettingsCoreTests(unittest.TestCase):
    def test_native_settings_core_contracts(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            environment = os.environ.copy()
            environment["CLANG_MODULE_CACHE_PATH"] = str(work / "clang-cache")
            environment["SWIFT_MODULECACHE_PATH"] = str(work / "swift-cache")
            sdk = subprocess.check_output(
                ["xcrun", "--show-sdk-path"], text=True, timeout=15
            ).strip()
            # Execute @main with the installed Apple toolchain, without asking
            # the OS to trust a newly generated standalone test executable.
            run_result = subprocess.run(
                [
                    "xcrun",
                    "swift", "-frontend", "-interpret", "-parse-as-library",
                    "-sdk", sdk, "-target", "arm64-apple-macos13.0",
                    str(ROOT / "test" / "native_settings_core_harness.swift"),
                    str(ROOT / "native_settings" / "SettingsCore.swift"),
                    str(ROOT / "native_settings" / "SpeechModels.swift"),
                    str(ROOT / "native_settings" / "SupervisorHealth.swift"),
                ],
                capture_output=True,
                text=True,
                timeout=90,
                check=False,
                env=environment,
            )
            self.assertEqual(run_result.returncode, 0, run_result.stderr)
            self.assertIn("Native settings core checks passed", run_result.stdout)


if __name__ == "__main__":
    unittest.main()
