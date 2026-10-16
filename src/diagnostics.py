"""Private non-content runtime diagnostics for the native settings app."""

from __future__ import annotations

import json
import os
import tempfile
import threading
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

from .runtime_paths import DIAGNOSTICS_FILE


class DiagnosticsStore:
    def __init__(self, path: Optional[Path] = None, *, durable: bool = True) -> None:
        self.path = path or DIAGNOSTICS_FILE
        self.durable = durable
        self._lock = threading.RLock()

    def update(self, **values: Any) -> None:
        with self._lock:
            try:
                data = json.loads(self.path.read_text(encoding="utf-8"))
            except (OSError, UnicodeError, json.JSONDecodeError):
                data = {}
            if not isinstance(data, dict):
                data = {}
            data.update(values)
            data["updated_at"] = datetime.now(timezone.utc).astimezone().isoformat(
                timespec="seconds"
            )
            self._save(data)

    def _save(self, data: dict[str, Any]) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        fd, temporary_name = tempfile.mkstemp(prefix=f".{self.path.name}.", dir=self.path.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                json.dump(data, handle, ensure_ascii=False, indent=2)
                handle.write("\n")
                handle.flush()
                if self.durable:
                    os.fsync(handle.fileno())
            os.chmod(temporary_name, 0o600)
            os.replace(temporary_name, self.path)
        finally:
            if os.path.exists(temporary_name):
                os.unlink(temporary_name)
