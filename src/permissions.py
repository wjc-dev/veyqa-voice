"""macOS permission status and request bridge for the running app identity.

The native settings window is an embedded helper with its own executable
identity.  It must not query or request TCC permissions for itself.  Instead it
writes a tiny request file and this module handles it inside the actual Voice
Input process.  Consequently every status shown in Settings belongs to the
same executable that listens for hotkeys, records audio, and inserts text.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .runtime_paths import (
    PERMISSIONS_FILE,
    PERMISSION_REQUEST_FILE,
    app_bundle_path,
)
from .utils.logger import logger


def _microphone_status() -> str:
    try:
        import AVFoundation

        status = int(
            AVFoundation.AVCaptureDevice.authorizationStatusForMediaType_(
                AVFoundation.AVMediaTypeAudio
            )
        )
        return {
            int(AVFoundation.AVAuthorizationStatusAuthorized): "granted",
            int(AVFoundation.AVAuthorizationStatusDenied): "denied",
            int(AVFoundation.AVAuthorizationStatusRestricted): "restricted",
            int(AVFoundation.AVAuthorizationStatusNotDetermined): "not_determined",
        }.get(status, "unknown")
    except Exception as exc:  # pragma: no cover - framework availability is OS-specific
        logger.debug("无法读取麦克风权限: %s", exc)
        return "unavailable"


def _accessibility_status() -> str:
    try:
        import ApplicationServices

        return "granted" if ApplicationServices.AXIsProcessTrusted() else "missing"
    except Exception as exc:  # pragma: no cover - framework availability is OS-specific
        logger.debug("无法读取辅助功能权限: %s", exc)
        return "unavailable"


def _input_monitoring_status() -> str:
    try:
        import Quartz

        return "granted" if Quartz.CGPreflightListenEventAccess() else "missing"
    except Exception as exc:  # pragma: no cover - framework availability is OS-specific
        logger.debug("无法读取输入监控权限: %s", exc)
        return "unavailable"


def required_permissions_granted(
    microphone: str,
    accessibility: str,
    input_monitoring: str,
    *,
    input_monitoring_required: bool,
) -> bool:
    required = microphone == "granted" and accessibility == "granted"
    if input_monitoring_required:
        required = required and input_monitoring == "granted"
    return required


def _executable_fingerprint(path: Path) -> str:
    try:
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()[:16]
    except OSError:
        return "unknown"


def _bundle_identifier(bundle: Path | None) -> str:
    if bundle is None:
        return "source.python"
    plist = bundle / "Contents" / "Info.plist"
    try:
        import plistlib

        with plist.open("rb") as handle:
            return str(plistlib.load(handle).get("CFBundleIdentifier") or "unknown")
    except (OSError, ValueError):
        return "unknown"


class PermissionMonitor:
    """Publish status and execute requests as the current Veyqa process."""

    def __init__(
        self,
        *,
        version: str,
        hotkey_backend: str,
        status_path: Path | None = None,
        request_path: Path | None = None,
        interval: float = 2.0,
    ) -> None:
        self.version = version
        self.hotkey_backend = hotkey_backend
        self.status_path = status_path or PERMISSIONS_FILE
        self.request_path = request_path or PERMISSION_REQUEST_FILE
        self.interval = max(0.25, interval)
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._request_lock = threading.Lock()
        self._last_refresh = float("-inf")
        self._last_status: dict[str, Any] | None = None
        self._last_snapshot: dict[str, Any] | None = None

        self.bundle = app_bundle_path()
        self.executable = Path(sys.executable).resolve()
        self.identity = {
            "version": version,
            "bundle_identifier": _bundle_identifier(self.bundle),
            "bundle_path": str(self.bundle or self.executable),
            "executable_path": str(self.executable),
            "executable_fingerprint": _executable_fingerprint(self.executable),
            "pid": os.getpid(),
        }

    def start(self) -> None:
        if self._thread and self._thread.is_alive():
            return
        self.refresh()
        self._thread = threading.Thread(
            target=self._run,
            name="permission-monitor",
            daemon=True,
        )
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()

    @property
    def latest_snapshot(self) -> dict[str, Any] | None:
        """A published copy; reading it never performs a new system permission query."""
        snapshot = self._last_snapshot
        return dict(snapshot) if snapshot is not None else None

    def refresh(self) -> dict[str, Any]:
        microphone = _microphone_status()
        accessibility = _accessibility_status()
        input_monitoring = _input_monitoring_status()
        input_required = self.hotkey_backend == "passive"
        snapshot: dict[str, Any] = {
            **self.identity,
            "microphone": microphone,
            "accessibility": accessibility,
            "input_monitoring": input_monitoring,
            "input_monitoring_required": input_required,
            "all_required_granted": required_permissions_granted(
                microphone,
                accessibility,
                input_monitoring,
                input_monitoring_required=input_required,
            ),
            "checked_by_current_process": True,
        }
        if snapshot != self._last_status or not self.status_path.exists():
            published = {**snapshot, "updated_at": datetime.now(timezone.utc).astimezone().isoformat(timespec="seconds")}
            self._atomic_write(self.status_path, published)
            self._last_status = snapshot
            self._last_snapshot = published
        self._last_refresh = time.monotonic()
        return dict(self._last_snapshot or snapshot)

    def _settings_active(self) -> bool:
        try:
            pid = int((self.status_path.parent / ".settings-instance").read_text().strip())
            if pid <= 1:
                return False
            os.kill(pid, 0)
            return True
        except (OSError, ValueError):
            return False

    def _poll(self) -> None:
        requested = self.request_path.exists()
        if requested:
            self._consume_request()
        now = time.monotonic()
        refresh_interval = self.interval if self._settings_active() else 30.0
        if requested or now - self._last_refresh >= refresh_interval:
            self.refresh()
            self._last_refresh = now

    def _run(self) -> None:
        while not self._stop.wait(self.interval):
            try:
                self._poll()
            except Exception:
                logger.exception("权限状态更新失败，将自动重试")

    def _consume_request(self) -> None:
        if not self.request_path.exists() or not self._request_lock.acquire(blocking=False):
            return
        try:
            try:
                request = json.loads(self.request_path.read_text(encoding="utf-8"))
            except (OSError, UnicodeError, json.JSONDecodeError):
                return
            finally:
                try:
                    self.request_path.unlink(missing_ok=True)
                except OSError:
                    pass
            permission = str(request.get("permission", "all"))
            open_settings = bool(request.get("open_settings", False))
            self._request(permission, open_settings=open_settings)
        finally:
            self._request_lock.release()

    def _request(self, permission: str, *, open_settings: bool) -> None:
        required = ["microphone", "accessibility"]
        if self.hotkey_backend == "passive":
            required.append("input_monitoring")
        requested = required if permission == "all" else [permission]
        for item in requested:
            if item not in required or self._current_status(item) == "granted":
                continue
            if item == "microphone":
                self._request_microphone()
            elif item == "accessibility":
                self._request_accessibility()
            elif item == "input_monitoring":
                self._request_input_monitoring()
        if (
            open_settings
            and permission != "all"
            and permission in required
            and self._current_status(permission) != "granted"
        ):
            self._open_settings(permission)

    @staticmethod
    def _current_status(permission: str) -> str:
        if permission == "microphone":
            return _microphone_status()
        if permission == "accessibility":
            return _accessibility_status()
        if permission == "input_monitoring":
            return _input_monitoring_status()
        return "unknown"

    @staticmethod
    def _request_microphone() -> None:
        try:
            import AVFoundation

            if _microphone_status() != "not_determined":
                return
            finished = threading.Event()

            def completion(_granted: bool) -> None:
                finished.set()
                return None

            AVFoundation.AVCaptureDevice.requestAccessForMediaType_completionHandler_(
                AVFoundation.AVMediaTypeAudio,
                completion,
            )
            finished.wait(60)
        except Exception as exc:  # pragma: no cover - framework availability is OS-specific
            logger.warning("无法请求麦克风权限: %s", exc)

    @staticmethod
    def _request_accessibility() -> None:
        try:
            import ApplicationServices

            ApplicationServices.AXIsProcessTrustedWithOptions(
                {ApplicationServices.kAXTrustedCheckOptionPrompt: True}
            )
        except Exception as exc:  # pragma: no cover - framework availability is OS-specific
            logger.warning("无法请求辅助功能权限: %s", exc)

    @staticmethod
    def _request_input_monitoring() -> None:
        try:
            import Quartz

            Quartz.CGRequestListenEventAccess()
        except Exception as exc:  # pragma: no cover - framework availability is OS-specific
            logger.warning("无法请求输入监控权限: %s", exc)

    @staticmethod
    def _open_settings(permission: str) -> None:
        pane = {
            "microphone": "Privacy_Microphone",
            "accessibility": "Privacy_Accessibility",
            "input_monitoring": "Privacy_ListenEvent",
        }.get(permission)
        if not pane:
            return
        try:
            subprocess.Popen(  # noqa: S603 - fixed executable and allowlisted URL
                [
                    "/usr/bin/open",
                    f"x-apple.systempreferences:com.apple.preference.security?{pane}",
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        except OSError as exc:
            logger.warning("无法打开系统权限设置: %s", exc)

    @staticmethod
    def _atomic_write(path: Path, data: dict[str, Any]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                json.dump(data, handle, ensure_ascii=False, indent=2)
                handle.write("\n")
                handle.flush()
                os.fsync(handle.fileno())
            os.chmod(temporary_name, 0o600)
            os.replace(temporary_name, path)
        finally:
            if os.path.exists(temporary_name):
                os.unlink(temporary_name)
