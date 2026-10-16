import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class NativeSettingsCoreTests(unittest.TestCase):
    def test_two_custom_combos_validate_and_persist_through_swift_core(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            binary = work / "settings-core-test"
            environment = os.environ.copy()
            environment["CLANG_MODULE_CACHE_PATH"] = str(work / "clang-cache")
            environment["SWIFT_MODULECACHE_PATH"] = str(work / "swift-cache")
            compile_result = subprocess.run(
                [
                    "xcrun",
                    "swiftc",
                    str(ROOT / "native_settings" / "SettingsCore.swift"),
                    str(ROOT / "native_settings" / "SupervisorHealth.swift"),
                    str(ROOT / "test" / "native_settings_core_harness.swift"),
                    "-o",
                    str(binary),
                ],
                capture_output=True,
                text=True,
                timeout=90,
                check=False,
                env=environment,
            )
            self.assertEqual(compile_result.returncode, 0, compile_result.stderr)
            run_result = subprocess.run(
                [str(binary)],
                capture_output=True,
                text=True,
                timeout=15,
                check=False,
            )
            self.assertEqual(run_result.returncode, 0, run_result.stderr)


if __name__ == "__main__":
    unittest.main()
