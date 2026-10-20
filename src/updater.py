"""Small main-thread adapter for the native Sparkle controller.

Only explicit settings commands cross the process boundary. There are no
download URLs, credentials, executable paths, or client-written install scripts.
"""
from __future__ import annotations

import ctypes
import json
import os
import time
from pathlib import Path

from src.diagnostics import DiagnosticsStore


class UpdateCommands:
    def __init__(self, directory: Path):
        self.path = directory / ".update-request.json"
        self.last_id = ""

    def take(self, now: float | None = None) -> dict | None:
        try:
            stat = self.path.stat()
            if stat.st_size > 1024 or stat.st_uid != os.getuid() or self.path.is_symlink():
                return None
            request = json.loads(self.path.read_text())
            self.path.unlink(missing_ok=True)
            identifier = request.get("id")
            timestamp = request.get("created_at")
            if (not isinstance(identifier, str) or not 1 <= len(identifier) <= 64
                    or identifier == self.last_id or isinstance(timestamp, bool)
                    or not isinstance(timestamp, (int, float))
                    or not 0 <= (time.time() if now is None else now) - timestamp <= 300):
                return None
            action = request.get("action")
            if action not in {"check", "automatic"}:
                return None
            if set(request) - {"id", "created_at", "action", "enabled"}:
                return None
            if action == "automatic" and type(request.get("enabled")) is not bool:
                return None
            self.last_id = identifier
            return request
        except (OSError, ValueError, AttributeError, TypeError):
            return None


def settings_block_install(directory: Path) -> bool:
    """Ignore stale locks left by a settings crash; never discard a live draft."""
    try:
        state = json.loads((directory / ".settings-update-state.json").read_text())
        pid = state["pid"]
        if type(pid) is not int or pid <= 1:
            return True
        os.kill(pid, 0)
        return state.get("dirty") is not False
    except ProcessLookupError:
        return False
    except FileNotFoundError:
        return False
    except (OSError, ValueError, KeyError, TypeError):
        return True


class AppUpdater:
    def __init__(self, app_path: Path, directory: Path, gate):
        import objc
        self.commands = UpdateCommands(directory)
        self.directory = directory
        self.library = ctypes.CDLL(str(app_path / "Contents/Frameworks/VeyqaUpdates.dylib"))
        controller = objc.lookUpClass("VeyqaUpdateController")
        self.native = controller.alloc().initWithBundlePath_dataPath_gate_(
            str(app_path), str(directory), gate)
        if self.native is None:
            raise RuntimeError("无法初始化应用更新")
        self.native.start()

    def tick(self):
        request = self.commands.take()
        if request:
            if request["action"] == "check":
                self.native.checkForUpdates()
            else:
                self.native.setAutomaticChecks_(request["enabled"])
        self.native.resumeIfReady()

    def check(self):
        self.native.checkForUpdates()


def unavailable(directory: Path):
    DiagnosticsStore(directory / "updates.json", durable=False).update(
        status="暂时无法检查更新，请从官网下载新版", available=False,
        automatic=False, checking=False, version="", build="")
