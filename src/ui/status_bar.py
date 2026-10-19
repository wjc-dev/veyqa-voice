"""macOS app controller for the resident Dock entry and settings window."""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import threading
from datetime import datetime
from dataclasses import dataclass
from typing import Dict, Optional, Tuple

from AppKit import (
    NSEventMaskLeftMouseUp,
    NSEventMaskRightMouseUp,
    NSEventTypeRightMouseUp,
    NSImageOnly,
    NSImageScaleProportionallyDown,
)
from Cocoa import (
    NSApplication,
    NSApplicationActivateAllWindows,
    NSApplicationActivateIgnoringOtherApps,
    NSApplicationActivationPolicyRegular,
    NSImage,
    NSRunningApplication,
    NSSquareStatusItemLength,
    NSStatusBar,
)
from Foundation import NSObject
import objc
from PyObjCTools import AppHelper

from src.keyboard.inputState import InputState
from src.runtime_paths import DATA_DIR, IS_FROZEN, LOG_DIR, RESOURCE_ROOT, app_bundle_path


_settings_process_lock = threading.RLock()
_settings_process: Optional[subprocess.Popen] = None
_status_log_lock = threading.RLock()
_STATUS_LOG_MAX_BYTES = 512 * 1024


@dataclass(frozen=True)
class _StateVisual:
    fallback_text: str
    description: str
    env_key: str


_STATE_VISUALS = {
    InputState.IDLE: _StateVisual("🎙️", "空闲", "IDLE"),
    InputState.RECORDING: _StateVisual("🔴", "正在启动录音", "RECORDING"),
    InputState.STREAMING: _StateVisual("🟢", "实时识别中", "RECORDING"),
    InputState.PROCESSING: _StateVisual("🔵", "正在生成最终结果", "PROCESSING"),
    InputState.WARNING: _StateVisual("⚠️", "警告", "PROCESSING"),
    InputState.ERROR: _StateVisual("❗️", "错误", "PROCESSING"),
}


def _service_label() -> str:
    return "豆包" if os.getenv("TRANSCRIPTION_SERVICE", "qwen").lower() == "doubao" else "千问"


def _menu_bar_enabled() -> bool:
    """Keep the legacy menu-bar entry opt-in instead of making it the only UI."""
    return os.getenv("SHOW_MENU_BAR_ICON", "false").lower() == "true"


def _write_status_log(message: str) -> None:
    # Diagnostics must never be able to terminate the resident app. Keep the
    # helper log bounded and serialize rotation with writes from watcher threads.
    try:
        with _status_log_lock:
            os.makedirs(LOG_DIR, exist_ok=True)
            log_path = LOG_DIR / "settings_ui.log"
            if log_path.exists() and log_path.stat().st_size >= _STATUS_LOG_MAX_BYTES:
                rotated = LOG_DIR / "settings_ui.log.1"
                rotated.unlink(missing_ok=True)
                os.replace(log_path, rotated)
            with open(log_path, "a", encoding="utf-8") as log_file:
                log_file.write(
                    f"{datetime.now().isoformat(timespec='seconds')} {message}\n"
                )
                log_file.flush()
    except OSError:
        pass


def _current_event_type():
    event = NSApplication.sharedApplication().currentEvent()
    return event.type() if event is not None else None


def _activate_settings_process(process: subprocess.Popen) -> bool:
    if process.poll() is not None:
        return False
    # applicationDidFinishLaunching writes this lock only after installing the
    # SIGUSR1 source.  A rapid second click before that handshake used to send
    # SIGUSR1 to the default signal action and kill the helper (exit -30).
    instance_path = DATA_DIR / ".settings-instance"
    try:
        ready_pid = int(instance_path.read_text(encoding="utf-8").strip())
    except (OSError, ValueError):
        ready_pid = 0
    if ready_pid != process.pid:
        _write_status_log(f"settings process warming up pid={process.pid}")
        return True
    # The native settings helper is a Swift executable rather than a bundled
    # NSRunningApplication. Its SIGUSR1 handler is the most reliable way to
    # raise the existing window even when macOS does not expose it here.
    try:
        os.kill(process.pid, signal.SIGUSR1)
    except OSError:
        return False
    try:
        running = NSRunningApplication.runningApplicationWithProcessIdentifier_(process.pid)
        if running is None:
            return True
        options = NSApplicationActivateIgnoringOtherApps | NSApplicationActivateAllWindows
        running.unhide()
        running.activateWithOptions_(options)
        return True
    except Exception as exc:  # noqa: BLE001
        _write_status_log(f"settings activation failed: {exc}")
        return False


def _launch_settings_window(on_restart=None) -> int:
    """Launch the visual settings window and keep a small diagnostic log."""
    global _settings_process
    with _settings_process_lock:
        if _settings_process is not None and _activate_settings_process(_settings_process):
            _write_status_log(f"reused settings process pid={_settings_process.pid}")
            return _settings_process.pid

    root_dir = str(RESOURCE_ROOT)
    settings_script = os.path.join(root_dir, "settings_ui.py")
    native_source = os.path.join(root_dir, "native_settings", "VoiceInputSettings.swift")
    native_core = os.path.join(root_dir, "native_settings", "SettingsCore.swift")
    native_info = os.path.join(
        root_dir, "native_settings", "VoiceInputSettings-Info.plist"
    )
    containing_bundle = app_bundle_path() if IS_FROZEN else None
    if containing_bundle is not None:
        native_binary = str(
            containing_bundle
            / "Contents"
            / "Helpers"
            / "VeyqaSettings.app"
            / "Contents"
            / "MacOS"
            / "VeyqaSettings"
        )
    else:
        native_binary = os.path.join(
            root_dir,
            "build",
            "VeyqaSettings.app",
            "Contents",
            "MacOS",
            "VeyqaSettings",
        )
    log_dir = str(LOG_DIR)
    log_path = os.path.join(log_dir, "settings_ui.log")
    os.makedirs(log_dir, exist_ok=True)

    executable = sys.executable
    arguments = [sys.executable, settings_script]
    try:
        needs_build = not IS_FROZEN and (
            not os.path.exists(native_binary)
            or any(
                os.path.getmtime(source) > os.path.getmtime(native_binary)
                for source in (native_source, native_core, native_info)
            )
        )
        if needs_build:
            build_result = subprocess.run(
                [os.path.join(root_dir, "build-native-settings.command")],
                cwd=root_dir,
                capture_output=True,
                text=True,
                timeout=90,
                check=False,
            )
            if build_result.returncode != 0:
                raise RuntimeError(build_result.stderr.strip() or "SwiftUI 设置编译失败")
        executable = native_binary
        arguments = [native_binary]
    except Exception as exc:  # noqa: BLE001
        # 没有 Swift 工具链时保留 PyQt 版作为兼容兜底。
        _write_status_log(f"native settings unavailable, fallback to PyQt: {exc}")

    _write_status_log(f"launch requested executable={executable}")
    child_env = os.environ.copy()
    child_env["VOICE_INPUT_ROOT"] = root_dir
    child_env["VOICE_INPUT_DATA_ROOT"] = str(DATA_DIR)
    child_env["VOICE_INPUT_BUNDLED"] = "true" if IS_FROZEN else "false"
    child_env["VOICE_INPUT_PARENT_PID"] = str(os.getpid())
    if containing_bundle is not None:
        child_env["VOICE_INPUT_APP_PATH"] = str(containing_bundle)
    try:
        with open(log_path, "a", encoding="utf-8") as log_file:
            process = subprocess.Popen(
                arguments,
                cwd=root_dir,
                env=child_env,
                start_new_session=True,
                stdout=log_file,
                stderr=log_file,
            )
    except Exception:
        raise
    with _settings_process_lock:
        _settings_process = process
    _write_status_log(f"settings process started pid={process.pid}")

    def _watch_settings() -> None:
        global _settings_process
        return_code = process.wait()
        _write_status_log(f"settings process exited status={return_code}")
        with _settings_process_lock:
            if _settings_process is process:
                _settings_process = None
        if return_code == 42 and on_restart is not None:
            AppHelper.callAfter(on_restart)

    threading.Thread(
        target=_watch_settings,
        name="settings-process-watch",
        daemon=True,
    ).start()
    # Do not signal a process that has only just been spawned. The Swift helper
    # installs its SIGUSR1 handler in applicationDidFinishLaunching; sending the
    # signal before then terminates it with status -SIGUSR1. The helper already
    # activates and raises its own window after launch. Subsequent clicks reuse
    # the live process through _activate_settings_process above.
    return process.pid


class _MenuActionHandler(NSObject):
    """Objective-C target for status-menu actions."""

    @objc.IBAction
    def handleStatusItem_(self, _sender) -> None:
        event_type = _current_event_type()
        if event_type == NSEventTypeRightMouseUp:
            _write_status_log("status item right-clicked")
            _launch_settings_window(getattr(self, "on_restart", None))
            return
        _write_status_log("status item left-clicked")
        _launch_settings_window(getattr(self, "on_restart", None))


class _ApplicationDelegate(NSObject):
    """Make a second Finder/Dock launch reopen the already-running settings."""

    def applicationShouldHandleReopen_hasVisibleWindows_(self, _app, _visible) -> bool:
        _write_status_log("dock reopen requested")
        self.controller._open_settings()
        return True

    def applicationShouldTerminate_(self, _app):
        # Dock Quit / Cmd-Q is intentional, unlike a signal from the supervisor.
        from src.login_item import SUPERVISOR_PAUSE_FILE
        try:
            SUPERVISOR_PAUSE_FILE.write_text("paused-by-user", encoding="utf-8")
        except OSError:
            pass
        return 1  # NSTerminateNow


class StatusBarController:
    """Keep the app resident in the Dock and own its optional menu-bar item."""

    def __init__(
        self,
        *,
        on_restart=None,
        open_settings_on_start: bool = False,
    ) -> None:
        self._status_item = None
        self._menu = None
        self._menu_action_handler = None
        self._current_state: InputState = InputState.IDLE
        self._queue_length: int = 0
        self._on_restart = on_restart
        self._open_settings_on_start = open_settings_on_start
        self._application_delegate = None
        self._status_health_check_scheduled = False

        self._custom_icons: Dict[str, NSImage] = {}
        self._load_custom_icons()

    def start(self) -> None:
        """Start the resident AppKit application and enter its event loop."""
        AppHelper.callAfter(self._setup)
        # A full NSApplication event loop is required for Dock reopen events,
        # the optional status item, and the settings helper lifecycle.
        AppHelper.runEventLoop()

    def update_state(
        self,
        state: InputState,
        *,
        queue_length: int = 0,
    ) -> None:
        """更新状态显示"""

        queue_length = max(0, queue_length)

        def _apply() -> None:
            self._current_state = state
            self._queue_length = queue_length
            self._refresh()

        AppHelper.callAfter(_apply)

    def open_settings(self) -> None:
        """Open or raise settings from Finder, the Dock, or the optional menu bar."""
        self._open_settings()

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _setup(self) -> None:
        app = NSApplication.sharedApplication()
        # The Dock is the stable primary entry point. A menu-bar item can be
        # hidden by macOS when space is tight, so it must never be the user's
        # only way back into settings.
        app.setActivationPolicy_(NSApplicationActivationPolicyRegular)
        # A bundled Python runtime can inherit its framework's generic icon.
        # Resolve the signed outer bundle's asset explicitly, without resetting
        # the user's Dock or global LaunchServices caches.
        bundle = app_bundle_path()
        if bundle:
            icon = NSImage.alloc().initWithContentsOfFile_(
                str(bundle / "Contents/Resources/AppIcon.icns")
            )
            if icon is not None:
                app.setApplicationIconImage_(icon)
                _write_status_log("dock application icon loaded from main bundle")
        self._application_delegate = _ApplicationDelegate.alloc().init()
        self._application_delegate.controller = self
        app.setDelegate_(self._application_delegate)

        if _menu_bar_enabled():
            self._install_status_item()
            self._schedule_status_health_check()
        else:
            _write_status_log("dock entry ready; menu bar disabled")
        if self._open_settings_on_start:
            AppHelper.callLater(
                0.8,
                self._open_startup_settings,
            )
        if os.getenv("DOCK_ENTRY_SELF_TEST", "false").lower() == "true":
            AppHelper.callLater(1.2, self._open_settings)

    def _open_startup_settings(self):
        # A delayed startup callback must never steal a dictation's target.
        if self._current_state == InputState.IDLE:
            _launch_settings_window(self._on_restart)

    def _install_status_item(self) -> None:
        """Create one retained, fixed-size status item and wire its action."""
        status_bar = NSStatusBar.systemStatusBar()
        old_item = self._status_item
        if old_item is not None:
            try:
                status_bar.removeStatusItem_(old_item)
            except Exception:  # noqa: BLE001 - invalid AppKit handles are recoverable
                pass

        self._status_item = status_bar.statusItemWithLength_(NSSquareStatusItemLength)
        try:
            self._status_item.setVisible_(True)
        except Exception:  # noqa: BLE001 - setVisible is unavailable on very old macOS
            pass

        button = self._status_item.button()
        if button is None:
            _write_status_log("status item creation failed: button unavailable")
            return

        button.setTitle_(f"🎙 {_service_label()}")
        button.setToolTip_("Voice Input - 点击打开设置")
        button.setEnabled_(True)
        if self._menu_action_handler is None:
            self._menu_action_handler = _MenuActionHandler.alloc().init()
        self._menu_action_handler.on_restart = self._on_restart
        button.setTarget_(self._menu_action_handler)
        button.setAction_(self._menu_action_handler.handleStatusItem_)
        button.sendActionOn_(NSEventMaskLeftMouseUp | NSEventMaskRightMouseUp)
        self._refresh()
        if os.getenv("STATUS_BAR_SELF_TEST", "false").lower() == "true":
            AppHelper.callLater(1.0, button.performClick_, None)
        _write_status_log("status item ready (fixed square item)")

    def _schedule_status_health_check(self) -> None:
        if self._status_health_check_scheduled:
            return
        self._status_health_check_scheduled = True

        def _check() -> None:
            self._status_health_check_scheduled = False
            try:
                item = self._status_item
                button = item.button() if item is not None else None
                if item is None or button is None:
                    _write_status_log("status item lost; recreating")
                    self._install_status_item()
                else:
                    try:
                        if not item.isVisible():
                            item.setVisible_(True)
                            _write_status_log("status item visibility restored")
                    except Exception:  # noqa: BLE001
                        pass
                    self._refresh()
            finally:
                self._schedule_status_health_check()

        AppHelper.callLater(3.0, _check)

    def _open_settings(self) -> None:
        _launch_settings_window(
            self._on_restart,
        )

    def _refresh(self) -> None:
        if self._status_item is None:
            return
        button = self._status_item.button()
        if button is None:
            return

        title, image, tooltip = self._icon_and_tooltip()

        if image is not None:
            image.setSize_((18.0, 18.0))
            button.setImage_(image)
            button.setTitle_(title)
            button.setImageScaling_(NSImageScaleProportionallyDown)
            button.setImagePosition_(NSImageOnly)
        else:
            button.setImage_(None)
            button.setTitle_(title)
            button.setImagePosition_(0)

        button.setToolTip_(tooltip)

    def _icon_and_tooltip(self) -> Tuple[str, Optional[NSImage], str]:
        visual = _STATE_VISUALS.get(self._current_state, _STATE_VISUALS[InputState.IDLE])

        image = self._custom_icons.get(visual.env_key)
        title = ""

        if image is None:
            title = visual.fallback_text

        tooltip = f"Voice Input - {visual.description}（点击打开设置）"
        if self._queue_length:
            tooltip += f" | 待处理任务 {self._queue_length}"

        return title, image, tooltip

    def _load_custom_icons(self) -> None:
        """Load the bundled monochrome PNG icon.

        SF Symbols were removed because on macOS Tahoe the symbol can decode
        to a representation with no pixel data (BPS=0, Pixels=0x0), which
        renders as a blank image and makes the status item invisible.
        """
        from src.runtime_paths import RESOURCE_ROOT

        candidates = [
            os.getenv("STATUS_ICON_PNG"),
            os.path.join(RESOURCE_ROOT, "StatusBarIcon.png"),
            os.path.join(RESOURCE_ROOT, "packaging", "StatusBarIcon.png"),
        ]
        icon_path = next((p for p in candidates if p and os.path.exists(p)), None)
        if icon_path is None:
            _write_status_log("status bar icon PNG not found")
            return

        image = NSImage.alloc().initWithContentsOfFile_(icon_path)
        if image is None:
            _write_status_log(f"status bar icon PNG failed to load: {icon_path}")
            return
        image.setTemplate_(True)
        # One icon for all states; we vary the tooltip text instead.  Color
        # changes are awkward in template mode and would defeat light/dark
        # adaptation.
        for visual in _STATE_VISUALS.values():
            self._custom_icons[visual.env_key] = image
        _write_status_log(f"status bar icon loaded: {icon_path}")
