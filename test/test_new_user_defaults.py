"""First-install defaults must never overwrite existing user settings."""
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from dotenv import dotenv_values
from src import runtime_paths
from src.keyboard.listener import KeyboardManager

ROOT = Path(__file__).resolve().parents[1]


class NewUserDefaultsTests(unittest.TestCase):
    def seed(self, directory, resource_root):
        root = Path(directory)
        with patch.multiple(runtime_paths, IS_FROZEN=True, DATA_DIR=root,
                            ENV_FILE=root / ".env", LOG_DIR=root / "logs",
                            RESOURCE_ROOT=resource_root):
            runtime_paths.ensure_runtime_layout()
        return root / ".env"

    def test_new_install_uses_right_option_hold_and_passive_backend(self):
        with tempfile.TemporaryDirectory() as directory:
            values = dotenv_values(self.seed(directory, ROOT))
        self.assertEqual(values["VOICE_HOTKEY"], "right_option")
        self.assertEqual(values["FN_HOTKEY_MODE"], "hold")
        self.assertEqual(values["SHOW_MENU_BAR_ICON"], "true")
        self.assertEqual(values["GLOBAL_HOTKEY_BACKEND"], "passive")

    def test_missing_template_has_same_new_defaults(self):
        with tempfile.TemporaryDirectory() as directory:
            values = dotenv_values(self.seed(directory, Path(directory) / "missing"))
        self.assertEqual(values["VOICE_HOTKEY"], "right_option")
        self.assertEqual(values["GLOBAL_HOTKEY_BACKEND"], "passive")

    def test_existing_config_is_byte_for_byte_unchanged(self):
        for text in ("VOICE_HOTKEY=left_command\nFN_HOTKEY_MODE=toggle\nQWEN_API_KEY=fixture\n",
                     "VOICE_HOTKEY=keycode:49;mods:control+option\n",
                     "# old implicit default\nQWEN_API_KEY=fixture\n"):
            with self.subTest(text=text), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / ".env"
                path.write_bytes(text.encode())
                self.seed(directory, ROOT)
                self.assertEqual(path.read_bytes(), text.encode())

    def test_missing_key_in_existing_config_keeps_legacy_default(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / ".env"
            path.touch()
            with patch("src.keyboard.listener.ENV_FILE", path):
                self.assertEqual(KeyboardManager._missing_hotkey_default(),
                                 "keycode:49;mods:control+option")

    def test_no_config_defaults_to_right_option(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch("src.keyboard.listener.ENV_FILE", Path(directory) / ".env"):
                self.assertEqual(KeyboardManager._missing_hotkey_default(), "right_option")

    def test_default_requires_input_monitoring_but_combinations_do_not(self):
        manager = KeyboardManager.__new__(KeyboardManager)
        self.assertEqual(KeyboardManager.DEFAULT_VOICE_HOTKEY, "right_option")
        spec = manager._parse_voice_hotkey(KeyboardManager.DEFAULT_VOICE_HOTKEY)
        self.assertEqual(manager._backend_for_hotkey(spec, "registered"), "passive")
        combo = manager._parse_voice_hotkey("keycode:49;mods:control+option")
        self.assertEqual(manager._backend_for_hotkey(combo, "passive"), "registered")
