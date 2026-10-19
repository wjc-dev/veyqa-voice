"""Resolve immutable app resources and writable per-user runtime data."""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path


SOURCE_ROOT = Path(__file__).resolve().parents[1]
IS_FROZEN = bool(getattr(sys, "frozen", False))
RESOURCE_ROOT = Path(getattr(sys, "_MEIPASS", SOURCE_ROOT))


def _default_data_dir() -> Path:
    override = os.getenv("VOICE_INPUT_DATA_DIR", "").strip()
    if override:
        return Path(override).expanduser()
    if IS_FROZEN:
        return Path.home() / "Library" / "Application Support" / "Veyqa"
    return SOURCE_ROOT


DATA_DIR = _default_data_dir()
ENV_FILE = DATA_DIR / ".env"
CONTEXT_FILE = DATA_DIR / "personal_context.txt"
CUSTOM_VOCABULARY_FILE = DATA_DIR / "custom_vocabulary.txt"
CORRECTIONS_FILE = DATA_DIR / "corrections.json"
RECOVERY_FILE = DATA_DIR / "recovery.json"
DIAGNOSTICS_FILE = DATA_DIR / "diagnostics.json"
PERMISSIONS_FILE = DATA_DIR / "permissions.json"
PERMISSION_REQUEST_FILE = DATA_DIR / ".permission-request.json"
AUDIO_ARCHIVE_DIR = DATA_DIR / "audio_archive"
LOG_DIR = DATA_DIR / "logs"


def ensure_runtime_layout() -> None:
    """Create private writable storage and seed a safe configuration template."""
    if IS_FROZEN and not DATA_DIR.exists():
        # A rebrand must not make users re-enter ASR credentials or lose their
        # shortcut and vocabulary. Migrate only configuration/state files, not
        # recordings or logs, from the previous app identities.
        for legacy_dir in (
            Path.home() / "Library" / "Application Support" / "Voice Input Next",
            Path.home() / "Library" / "Application Support" / "VoxType Next",
            Path.home() / "Library" / "Application Support" / "VoxType",
        ):
            if not legacy_dir.is_dir():
                continue
            DATA_DIR.mkdir(parents=True, exist_ok=True)
            for filename in (
                ".env",
                "personal_context.txt",
                "custom_vocabulary.txt",
                "corrections.json",
                "recovery.json",
            ):
                source = legacy_dir / filename
                destination = DATA_DIR / filename
                if source.is_file() and not destination.exists():
                    shutil.copy2(source, destination)
            break
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    try:
        DATA_DIR.chmod(0o700)
    except OSError:
        pass
    LOG_DIR.mkdir(parents=True, exist_ok=True)

    if IS_FROZEN and not ENV_FILE.exists():
        template = RESOURCE_ROOT / "env.example"
        if template.exists():
            shutil.copyfile(template, ENV_FILE)
        else:
            ENV_FILE.write_text(
                "TRANSCRIPTION_SERVICE=qwen\n"
                "QWEN_API_KEY=\n"
                "VOICE_HOTKEY=right_option\n"
                "VOICE_HOTKEY_LABEL=右 Option（⌥）\n"
                "FN_HOTKEY_MODE=hold\n"
                "GLOBAL_HOTKEY_BACKEND=passive\n"
                "SHOW_MENU_BAR_ICON=true\n"
                "PUNCTUATION_MODE=spaces\n"
                "AUDIO_ARCHIVE_ENABLED=false\n"
                "DISFLUENCY_FILTER_ENABLED=false\n"
                "DISFLUENCY_FILTER_MODE=off\n"
                "EXPERIMENTAL_CORRECTION_LEARNING=false\n"
                "CORRECTION_LEARNING_ENABLED=false\n"
                "CORRECTION_AUTO_REPLACE=false\n"
                "CORRECTION_REPLACE_MIN_COUNT=2\n"
                "CORRECTION_CONTEXT_ENABLED=false\n"
                "CORRECTION_CONTEXT_MIN_COUNT=2\n",
                encoding="utf-8",
            )
        try:
            ENV_FILE.chmod(0o600)
        except OSError:
            pass


def app_bundle_path() -> Path | None:
    """Return the containing .app path when running from a frozen bundle."""
    if not IS_FROZEN:
        return None
    executable = Path(sys.executable).resolve()
    for parent in executable.parents:
        if parent.suffix == ".app":
            return parent
    return None
