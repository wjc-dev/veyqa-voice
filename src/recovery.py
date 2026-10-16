"""Private bounded storage for recognized text that could not be committed."""

from __future__ import annotations

import fcntl
import json
import os
import tempfile
import threading
import time
import uuid
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

from .runtime_paths import RECOVERY_FILE


class RecoveryStore:
    def __init__(self, path: Optional[Path] = None, limit: int = 5) -> None:
        configured = os.getenv("RECOVERY_STORE_FILE", "").strip()
        self.path = path or (Path(configured).expanduser() if configured else RECOVERY_FILE)
        self.limit = max(1, min(20, limit))
        self._lock = threading.RLock()

    def load(self, *, strict: bool = False) -> list[dict[str, str]]:
        with self._lock:
            try:
                data = json.loads(self.path.read_text(encoding="utf-8"))
            except FileNotFoundError:
                return []
            except (OSError, UnicodeError, json.JSONDecodeError):
                if strict:
                    raise
                return []
            valid = isinstance(data, list) and all(
                isinstance(entry, dict) and all(
                    isinstance(entry.get(key), str) for key in ("id", "text", "reason", "timestamp")
                ) for entry in data
            )
            if not valid:
                if strict:
                    raise ValueError("恢复记录格式无效，原文件保持不变")
                return []
            return data[: self.limit]

    def add(self, text: str, reason: str) -> None:
        text = (text or "").strip()
        if not text:
            return
        with self._transaction():
            entries = self.load(strict=True)
            entries.insert(
                0,
                {
                    "id": uuid.uuid4().hex,
                    "text": text,
                    "reason": reason,
                    "timestamp": datetime.now(timezone.utc).astimezone().isoformat(
                        timespec="seconds"
                    ),
                },
            )
            self._save(entries[: self.limit])

    def delete(self, entry_id: str) -> bool:
        return self.delete_many({entry_id})

    def delete_many(self, entry_ids: set[str]) -> bool:
        """Delete only the selected IDs, preserving text added since UI refresh."""
        with self._transaction():
            entries = self.load(strict=True)
            updated = [entry for entry in entries if entry.get("id") not in entry_ids]
            if len(updated) == len(entries):
                return False
            self._save(updated)
            return True

    def clear(self) -> None:
        with self._transaction():
            self._save([])

    @contextmanager
    def _transaction(self):
        # A stable sidecar, not the atomically replaced JSON inode. The settings
        # helper uses the same flock protocol; thread-only locks are insufficient.
        with self._lock:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            lock_path = self.path.with_name(f".{self.path.name}.lock")
            fd = os.open(lock_path, os.O_CREAT | os.O_RDWR | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
            try:
                deadline = time.monotonic() + 2
                while True:
                    try:
                        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        break
                    except BlockingIOError:
                        if time.monotonic() >= deadline:
                            raise TimeoutError("恢复记录正在更新，请重试")
                        time.sleep(0.01)
                yield
            finally:
                os.close(fd)

    def _save(self, entries: list[dict[str, str]]) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        fd, temporary_name = tempfile.mkstemp(prefix=f".{self.path.name}.", dir=self.path.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                json.dump(entries, handle, ensure_ascii=False, indent=2)
                handle.write("\n")
                handle.flush()
                os.fsync(handle.fileno())
            os.chmod(temporary_name, 0o600)
            os.replace(temporary_name, self.path)
        finally:
            if os.path.exists(temporary_name):
                os.unlink(temporary_name)
