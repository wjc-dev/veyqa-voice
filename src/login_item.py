"""Modern macOS login-item registration for the bundled app.

The LaunchAgent supervises a tiny bundled helper, not Veyqa itself.  The
helper can recover an unexpectedly missing app while remaining independent of
normal app restarts.  This avoids the old pattern where launchd supervised
`/usr/bin/open`, which exits successfully before the real app has even started.
"""

from __future__ import annotations

import os
import json
import plistlib
import subprocess
from dataclasses import dataclass
from pathlib import Path

from src.runtime_paths import DATA_DIR, IS_FROZEN, app_bundle_path
from src.diagnostics import DiagnosticsStore
from src.utils.logger import logger


AGENT_PLIST_NAME = "com.wjcdev.veyqa.agent.plist"
SUPERVISOR_PAUSE_FILE = DATA_DIR / ".supervisor-paused"
REGISTRATION_FILE = DATA_DIR / "login-item-registration.json"
_STATUS_NAMES = {
    0: "not_registered",
    1: "enabled",
    2: "requires_approval",
    3: "not_found",
}
_LEGACY_AGENT_NAMES = (
    "com.voxtype.dev.agent.plist",
    "com.voxtype.dev.plist",
    "com.voxtype.dev.legacy.plist",
    "com.voiceinputnext.qwen.plist",
    "com.voiceinputnext.app.plist",
    "com.whisper-input-next.plist",
)


@dataclass(frozen=True)
class LoginItemResult:
    status: str
    changed: bool = False
    error: str = ""


def _status_name(value: int) -> str:
    return _STATUS_NAMES.get(int(value), f"unknown_{int(value)}")


def _registration_identity() -> str:
    bundle = app_bundle_path()
    if bundle is None:
        return ""
    with (bundle / "Contents/Info.plist").open("rb") as handle:
        build = plistlib.load(handle).get("CFBundleVersion", "")
    stat = bundle.stat()
    # Replacing a .app can leave SMAppService attached to the old bundle inode,
    # even when its status is still 'enabled'. Include reinstall as well as upgrade.
    return f"{bundle}:{build}:{stat.st_dev}:{stat.st_ino}"


def resume_supervisor_for_manual_launch(arguments: list[str]) -> None:
    """A Finder/Spotlight launch resumes service after an explicit user quit."""
    if "--supervised" in arguments:
        return
    try:
        SUPERVISOR_PAUSE_FILE.unlink(missing_ok=True)
    except OSError as exc:
        logger.warning("无法清除后台暂停标记: %s", exc)


def _retire_legacy_agents() -> None:
    """Remove only known obsolete launch plists after modern registration.

    User configuration and the old application bundle are deliberately left
    untouched.  A failed bootout is harmless because unlinking the known plist
    prevents it from returning at the next login.
    """
    domain = f"gui/{os.getuid()}"
    directory = Path.home() / "Library" / "LaunchAgents"
    for name in _LEGACY_AGENT_NAMES:
        path = directory / name
        if not path.exists():
            continue
        subprocess.run(
            ["/bin/launchctl", "bootout", domain, str(path)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=5,
            check=False,
        )
        try:
            path.unlink()
            logger.info("已停用旧版登录项：%s", name)
        except OSError as exc:
            logger.warning("无法移除旧版登录项 %s: %s", name, exc)


def _legacy_app_is_running() -> bool:
    """Detect old branded bundles without terminating another user process."""
    try:
        from AppKit import NSRunningApplication

        for identifier in ("com.voiceinputnext.qwen", "com.voiceinputnext.app"):
            if NSRunningApplication.runningApplicationsWithBundleIdentifier_(identifier):
                return True
    except Exception as exc:  # noqa: BLE001 - migration warning is best effort
        logger.debug("旧版进程检测不可用: %s", exc)
    return False


def sync_login_item(enabled: bool) -> LoginItemResult:
    """Make the bundled supervisor registration match the saved preference."""
    if os.getenv("VOICE_INPUT_DISABLE_LOGIN_SYNC", "false").lower() == "true":
        return LoginItemResult("disabled_for_test")
    if not IS_FROZEN:
        return LoginItemResult("source_run")

    try:
        import ServiceManagement as SM

        service = SM.SMAppService.agentServiceWithPlistName_(AGENT_PLIST_NAME)
        before = _status_name(service.status())
        changed = False

        if enabled:
            identity = _registration_identity()
            try:
                previous = json.loads(REGISTRATION_FILE.read_text()).get("identity", "")
            except (OSError, ValueError, AttributeError):
                previous = ""
            if identity and identity != previous and before in {"enabled", "requires_approval"}:
                ok, error = service.unregisterAndReturnError_(None)
                if not ok:
                    return LoginItemResult(before, error=str(error or "更新后台服务失败"))
                before = "not_registered"
                changed = True
            if before not in {"enabled", "requires_approval"}:
                ok, error = service.registerAndReturnError_(None)
                if not ok:
                    return LoginItemResult(before, error=str(error or "注册失败"))
                changed = True
            after = _status_name(service.status())
            if identity and after in {"enabled", "requires_approval"}:
                DiagnosticsStore(REGISTRATION_FILE).update(identity=identity)
            if after == "enabled":
                _retire_legacy_agents()
                if _legacy_app_is_running():
                    return LoginItemResult(
                        "enabled_legacy_running",
                        changed=changed,
                        error="检测到旧版 Voice Input 仍在运行；请先退出旧版，避免两个快捷键监听器同时工作。",
                    )
            return LoginItemResult(after, changed=changed)

        if before not in {"not_registered", "not_found"}:
            ok, error = service.unregisterAndReturnError_(None)
            if not ok:
                return LoginItemResult(before, error=str(error or "取消注册失败"))
            changed = True
        return LoginItemResult(_status_name(service.status()), changed=changed)
    except Exception as exc:  # noqa: BLE001 - startup must survive integration errors
        logger.error("后台启动服务配置失败: %s", exc)
        return LoginItemResult("error", error=str(exc))
