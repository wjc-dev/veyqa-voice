"""Signing selection tests: shell stubs only, no keys or native apps executed."""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
IDENTITY = "A" * 40


class BuildSigningTests(unittest.TestCase):
    def run_selection(self, directory, identity=None, available=True):
        env = dict(os.environ)
        env.pop("VEYQA_SIGNING_IDENTITY", None)
        env["VEYQA_SIGNING_CONFIG"] = str(Path(directory) / "identity.sha1")
        if identity is not None:
            env["VEYQA_SIGNING_IDENTITY"] = identity
        stub = f'print \'  1) {IDENTITY} "Veyqa Local Development"\'' if available else "return 1"
        return subprocess.run(
            ["/bin/zsh", "-c", f'''
                set -euo pipefail
                security() {{ {stub}; }}
                codesign() {{ printf '<%s>\\n' "$@"; }}
                source "$1/packaging/signing.zsh"
                veyqa_configure_signing
                veyqa_sign "/test folder/Veyqa Voice.app"
            ''', "test", str(ROOT)], env=env, text=True, capture_output=True, timeout=5,
        )

    def test_unconfigured_build_is_explicitly_internal(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_selection(directory)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("<->", result.stdout)
        self.assertIn("</test folder/Veyqa Voice.app>", result.stdout)

    def test_saved_local_identity_is_reused(self):
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, "identity.sha1").write_text(IDENTITY + "\n")
            result = self.run_selection(directory)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"<{IDENTITY}>", result.stdout)
        self.assertNotIn("<->", result.stdout)

    def test_missing_identity_never_falls_back_to_adhoc(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_selection(directory, IDENTITY, available=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Refusing ad-hoc fallback", result.stderr)
        self.assertNotIn("<--sign>", result.stdout)

    def test_invalid_or_empty_identity_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            for identity in ("", "Veyqa Local Development", "$(touch /tmp/not-run)"):
                with self.subTest(identity=identity):
                    result = self.run_selection(directory, identity)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("exact certificate SHA-1", result.stderr)
                    self.assertNotIn("<--sign>", result.stdout)

    def test_other_available_identity_is_not_silently_selected(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_selection(directory, "B" * 40)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Refusing ad-hoc fallback", result.stderr)

    def test_explicit_internal_build_does_not_change_saved_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory, "identity.sha1")
            config.write_text(IDENTITY + "\n")
            result = self.run_selection(directory, "-")
            self.assertEqual(config.read_text().strip(), IDENTITY)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("<->", result.stdout)
