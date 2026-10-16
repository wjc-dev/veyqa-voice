"""Global push-to-talk shortcut and safe cross-application text insertion.

This module intentionally contains no provider selection, translation mode,
provisional in-editor rewriting, or legacy status characters. The menu-bar
capsule owns live preview; the target editor receives one final commit only.
"""

from __future__ import annotations

import os
import queue
import subprocess
import sys
import threading
import time
import ctypes
from collections import deque
from pathlib import Path
from typing import Callable, Optional

from ..correction_learning import CorrectionLearner
from ..clipboard_paste import paste_text_preserving_clipboard
from ..runtime_paths import DATA_DIR
from ..recovery import RecoveryStore
from ..text_processing import sanitize_inline_text
from ..utils.logger import logger
from .inputState import InputState

_UNSET_TARGET = object()


class _PassiveTapListener:
    """Observe only the configured shortcut without swallowing keyboard input.

    Corporate endpoint protection can reasonably classify an active event tap
    as a keyboard interception capability. This listener uses Quartz's
    listen-only option:
    the callback can observe the selected shortcut but macOS will never allow
    it to modify or discard a user's key event.
    """

    def __init__(self, intercept, on_recovered=None, reconcile=None) -> None:
        self._intercept = intercept
        self._on_recovered = on_recovered or (lambda: None)
        self._reconcile = reconcile or (lambda: None)
        self._callback = self._deliver
        self.tap_ref = None
        self.run_loop = None
        self.running = False
        self.recovery_count = 0
        self.last_poll_at = time.monotonic()

    def __enter__(self):
        self.running = True
        return self

    def __exit__(self, _exc_type, _exc, _traceback) -> None:
        self.running = False

    def _deliver(self, _proxy, event_type, event, _refcon):
        # A listen-only event tap must always return the original event.  Even
        # if the shared handler returns None for the legacy suppressing mode,
        # this backend cannot interfere with Option, Fn, Command, or Escape.
        self._intercept(event_type, event)
        return event

    def _repair_if_disabled(self) -> bool:
        """Re-enable a listen-only tap that macOS disabled after a timeout.

        A disabled event tap is otherwise indistinguishable from an idle app:
        the process and menu-bar item stay alive, but the push-to-talk shortcut
        never fires again.  Polling the tap state gives that failure mode a
        deterministic recovery path.
        """
        from Quartz import CGEventTapEnable, CGEventTapIsEnabled

        tap = self.tap_ref
        if tap is None or CGEventTapIsEnabled(tap):
            return False
        CGEventTapEnable(tap, True)
        if not CGEventTapIsEnabled(tap):
            raise RuntimeError("快捷键监听无法重新启用")
        self._on_recovered()
        self.recovery_count += 1
        logger.warning("键盘事件监听被系统暂停，已自动恢复（第 %s 次）", self.recovery_count)
        return True

    def join(self) -> None:
        from Quartz import (
            CFMachPortCreateRunLoopSource,
            CFRunLoopAddSource,
            CFRunLoopGetCurrent,
            CFRunLoopRunInMode,
            CGEventMaskBit,
            CGEventTapCreate,
            CGEventTapEnable,
            kCFRunLoopCommonModes,
            kCFRunLoopDefaultMode,
            kCGEventFlagsChanged,
            kCGEventKeyDown,
            kCGEventKeyUp,
            kCGEventTapOptionListenOnly,
            kCGHeadInsertEventTap,
            kCGSessionEventTap,
        )

        event_mask = (
            CGEventMaskBit(kCGEventFlagsChanged)
            | CGEventMaskBit(kCGEventKeyDown)
            | CGEventMaskBit(kCGEventKeyUp)
        )
        tap = CGEventTapCreate(
            kCGSessionEventTap,
            kCGHeadInsertEventTap,
            kCGEventTapOptionListenOnly,
            event_mask,
            self._callback,
            None,
        )
        if tap is None:
            self.running = False
            raise RuntimeError("macOS 未允许只读快捷键监听")
        self.tap_ref = tap
        source = CFMachPortCreateRunLoopSource(None, tap, 0)
        self.run_loop = CFRunLoopGetCurrent()
        CFRunLoopAddSource(self.run_loop, source, kCFRunLoopCommonModes)
        CGEventTapEnable(tap, True)
        while self.running:
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, False)
            self.last_poll_at = time.monotonic()
            self._repair_if_disabled()
            self._reconcile()


class _PassiveGlobalMonitorListener:
    """Observe one shortcut through AppKit's read-only global monitor.

    Corporate endpoint software on the target machine repeatedly disables raw
    CGEventTap instances, even when they use the listen-only flag.  AppKit's
    managed monitor cannot suppress or rewrite events and avoids that unstable
    tap lifecycle while retaining modifier-only shortcut support.
    """

    def __init__(self, intercept) -> None:
        self._intercept = intercept
        self.monitor = None
        self.running = False

    def _deliver(self, event) -> None:
        self._intercept(event)

    def start(self) -> None:
        from AppKit import (
            NSEvent,
            NSEventMaskFlagsChanged,
            NSEventMaskKeyDown,
            NSEventMaskKeyUp,
        )

        mask = NSEventMaskFlagsChanged | NSEventMaskKeyDown | NSEventMaskKeyUp
        self.monitor = NSEvent.addGlobalMonitorForEventsMatchingMask_handler_(
            mask,
            self._deliver,
        )
        if self.monitor is None:
            raise RuntimeError("macOS 未允许只读快捷键监听")
        self.running = True

    def stop(self) -> None:
        if self.monitor is not None:
            from AppKit import NSEvent

            NSEvent.removeMonitor_(self.monitor)
        self.monitor = None
        self.running = False


class _RegisteredHotKeyListener:
    """Register one exact Carbon hotkey without observing other key events."""

    class _EventTypeSpec(ctypes.Structure):
        _fields_ = [("event_class", ctypes.c_uint32), ("event_kind", ctypes.c_uint32)]

    class _EventHotKeyID(ctypes.Structure):
        _fields_ = [("signature", ctypes.c_uint32), ("identifier", ctypes.c_uint32)]

    _EVENT_CLASS_KEYBOARD = int.from_bytes(b"keyb", "big")
    _EVENT_HOTKEY_PRESSED = 5
    _EVENT_HOTKEY_RELEASED = 6
    _CARBON_MODIFIERS = {
        "command": 1 << 8,
        "shift": 1 << 9,
        "option": 1 << 11,
        "control": 1 << 12,
        # Carbon uses the same secondary-Fn bit as CGEventFlags.
        "function": 1 << 23,
    }

    def __init__(self, spec, on_pressed) -> None:
        self.spec = spec
        self.on_pressed = on_pressed
        self.running = False
        self._library = None
        self._callback = None
        self._handler_ref = ctypes.c_void_p()
        self._hotkey_ref = ctypes.c_void_p()

    def _dispatch_kind(self, kind: int) -> int:
        if kind == self._EVENT_HOTKEY_PRESSED:
            self.on_pressed(True)
        elif kind == self._EVENT_HOTKEY_RELEASED:
            self.on_pressed(False)
        return 0

    def start(self) -> None:
        if self.spec.get("kind") != "key" or not self.spec.get("modifiers"):
            raise ValueError("系统注册型快捷键必须包含普通按键和至少一个修饰键")

        library = ctypes.CDLL("/System/Library/Frameworks/Carbon.framework/Carbon")
        callback_type = ctypes.CFUNCTYPE(
            ctypes.c_int32,
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.c_void_p,
        )

        library.GetApplicationEventTarget.restype = ctypes.c_void_p
        library.GetEventKind.argtypes = [ctypes.c_void_p]
        library.GetEventKind.restype = ctypes.c_uint32
        library.InstallEventHandler.argtypes = [
            ctypes.c_void_p,
            callback_type,
            ctypes.c_uint32,
            ctypes.POINTER(self._EventTypeSpec),
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_void_p),
        ]
        library.InstallEventHandler.restype = ctypes.c_int32
        library.RegisterEventHotKey.argtypes = [
            ctypes.c_uint32,
            ctypes.c_uint32,
            self._EventHotKeyID,
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.POINTER(ctypes.c_void_p),
        ]
        library.RegisterEventHotKey.restype = ctypes.c_int32

        def callback(_next_handler, event, _user_data):
            return self._dispatch_kind(int(library.GetEventKind(event)))

        self._callback = callback_type(callback)
        event_types = (self._EventTypeSpec * 2)(
            self._EventTypeSpec(self._EVENT_CLASS_KEYBOARD, self._EVENT_HOTKEY_PRESSED),
            self._EventTypeSpec(self._EVENT_CLASS_KEYBOARD, self._EVENT_HOTKEY_RELEASED),
        )
        target = library.GetApplicationEventTarget()
        status = library.InstallEventHandler(
            target,
            self._callback,
            len(event_types),
            event_types,
            None,
            ctypes.byref(self._handler_ref),
        )
        if status != 0:
            raise RuntimeError(f"无法安装系统快捷键处理器（{status}）")

        modifiers = 0
        for name in self.spec.get("modifiers", []):
            modifiers |= self._CARBON_MODIFIERS.get(name, 0)
        hotkey_id = self._EventHotKeyID(int.from_bytes(b"VInp", "big"), 1)
        status = library.RegisterEventHotKey(
            int(self.spec["vk"]),
            modifiers,
            hotkey_id,
            target,
            0,
            ctypes.byref(self._hotkey_ref),
        )
        if status != 0:
            raise RuntimeError(f"快捷键已被其他应用占用或注册失败（{status}）")
        self._library = library
        self.running = True


def _process_name(pid: int) -> str:
    try:
        result = subprocess.run(
            ["ps", "-p", str(pid), "-o", "comm="],
            capture_output=True,
            text=True,
            timeout=2,
            check=False,
        )
        return os.path.basename(result.stdout.strip()) or "未知进程"
    except Exception:
        return "未知进程"


class KeyboardManager:
    """Own one global shortcut and commit final text exactly once."""

    DEFAULT_VOICE_HOTKEY = "keycode:49;mods:control+option"
    DEFAULT_VOICE_HOTKEY_LABEL = "⌃⌥Space"
    VALID_REGISTERED_MODIFIERS = {
        "command",
        "shift",
        "option",
        "control",
        "function",
    }

    def __init__(
        self,
        on_record_start: Callable[[], None],
        on_record_stop: Callable[[], None],
        on_record_cancel: Optional[Callable[[], None]] = None,
        on_state_change: Optional[Callable[[InputState], None]] = None,
    ) -> None:
        self.on_record_start = on_record_start
        self.on_record_stop = on_record_stop
        self.on_record_cancel = on_record_cancel or (lambda: None)
        self.on_state_change = on_state_change

        self.correction_learner = CorrectionLearner()
        self.recovery_store = RecoveryStore()
        self._state = InputState.IDLE
        self.is_recording = False
        self._cancel_pending = False
        self._recording_lock = threading.RLock()
        self._last_voice_insertion = None
        self._recovery_texts = deque(maxlen=5)

        self.voice_hotkey = (
            os.getenv("VOICE_HOTKEY", self.DEFAULT_VOICE_HOTKEY).strip().lower()
            or self.DEFAULT_VOICE_HOTKEY
        )
        self.voice_hotkey_label = os.getenv("VOICE_HOTKEY_LABEL", "").strip()
        self._hotkey_mode = os.getenv("FN_HOTKEY_MODE", "hold").strip().lower()
        if self._hotkey_mode not in {"hold", "toggle"}:
            self._hotkey_mode = "hold"
        self._voice_hotkey_pressed = False
        self._voice_hotkey_spec = self._parse_voice_hotkey(self.voice_hotkey)
        if self._voice_hotkey_spec is None:
            logger.warning("快捷键配置无效，已恢复为 ⌃⌥Space")
            self.voice_hotkey = self.DEFAULT_VOICE_HOTKEY
            self.voice_hotkey_label = self.DEFAULT_VOICE_HOTKEY_LABEL
            self._voice_hotkey_spec = self._parse_voice_hotkey(
                self.DEFAULT_VOICE_HOTKEY
            )

        self._shortcut_capture_lock_file = DATA_DIR / ".shortcut-capture"
        self._shortcut_capture_checked_at = 0.0
        self._shortcut_capture_is_active = False
        self._listener = None
        self._tap_disabled_log_time = 0.0
        requested_backend = os.getenv(
            "GLOBAL_HOTKEY_BACKEND", "passive"
        ).strip().lower()
        self._hotkey_backend = self._backend_for_hotkey(
            self._voice_hotkey_spec,
            requested_backend,
        )
        self._suppress_vks, self._suppress_modifier_mask = self._build_hotkey_suppression()

        # Audio start/stop can block briefly. Serializing them off the event-tap
        # callback prevents a quick release from overtaking a slow start.
        self._transition_queue: queue.Queue[tuple[str, int]] = queue.Queue()
        self._transition_generation = 0
        self._transition_started_at = None
        self._transition_thread = threading.Thread(
            target=self._transition_worker,
            name="voice-state-transition",
            daemon=True,
        )
        self._transition_thread.start()

        mode = "按住说话" if self._hotkey_mode == "hold" else "按一下开始 / 再按一下结束"
        logger.info(
            "%s：%s",
            mode,
            self.voice_hotkey_label or self._voice_hotkey_display_label(),
        )

    @property
    def state(self) -> InputState:
        return self._state

    def set_state(self, state: InputState) -> None:
        if state == self._state:
            return
        self._state = state
        if self.on_state_change:
            try:
                self.on_state_change(state)
            except Exception as exc:  # noqa: BLE001
                logger.debug("状态回调异常: %s", exc)

    def show_warning(self, message: str) -> None:
        logger.warning("%s", message)
        self.set_state(InputState.WARNING)
        self._schedule_idle_reset()

    def show_error(self, message: str) -> None:
        logger.error("%s", message)
        self.set_state(InputState.ERROR)
        self._schedule_idle_reset()

    def _schedule_idle_reset(self) -> None:
        def reset_later() -> None:
            time.sleep(2)
            if self.state in {InputState.WARNING, InputState.ERROR}:
                self.reset_state()

        threading.Thread(target=reset_later, name="state-message-reset", daemon=True).start()

    def mark_streaming(self) -> None:
        self.set_state(InputState.STREAMING)

    def reset_state(self) -> None:
        with self._recording_lock:
            self.is_recording = False
        self.set_state(InputState.IDLE)

    def start_recording(self) -> None:
        with self._recording_lock:
            if (self.is_recording or not self.state.can_start_recording
                    or getattr(self, "_cancel_pending", False)
                    or getattr(self, "_transition_started_at", None) is not None):
                return
            self.is_recording = True
            self._transition_generation = getattr(self, "_transition_generation", 0) + 1
            self._transition_queue.put(("start", self._transition_generation))
        logger.info("🎤 开始录音")

    def stop_recording(self) -> None:
        with self._recording_lock:
            if not self.is_recording:
                return
            self.is_recording = False
            self._transition_queue.put(("stop", getattr(self, "_transition_generation", 0)))
        logger.info("⏹️ 停止录音")

    def cancel_recording(self) -> None:
        with self._recording_lock:
            if not self.is_recording:
                return
            self.is_recording = False
            self._cancel_pending = True
            self._transition_generation = getattr(self, "_transition_generation", 0) + 1
        # A blocked device start must not trap cancellation behind itself.
        def cancel():
            try:
                self.on_record_cancel()
            except Exception:
                logger.exception("取消录音失败")
            finally:
                self.reset_state()
                with self._recording_lock:
                    self._cancel_pending = False
        threading.Thread(target=cancel, name="voice-cancel", daemon=True).start()
        logger.info("已取消本轮语音输入")

    def toggle_recording(self) -> None:
        if self.is_recording:
            self.stop_recording()
        else:
            self.start_recording()

    def _transition_worker(self) -> None:
        while True:
            action, generation = self._transition_queue.get()
            self._transition_started_at = time.monotonic()
            self._active_transition_generation = generation
            try:
                if self.transition_was_cancelled():
                    continue
                if action == "start":
                    self.set_state(InputState.RECORDING)
                    self.on_record_start()
                elif action == "stop":
                    self.set_state(InputState.PROCESSING)
                    self.on_record_stop()
                elif action == "cancel":
                    self.on_record_cancel()
                    self.reset_state()
            except Exception as exc:  # noqa: BLE001
                logger.error("录音状态切换失败: %s", exc, exc_info=True)
                self.reset_state()
            finally:
                self._transition_started_at = None
                self._transition_queue.task_done()

    def transition_was_cancelled(self) -> bool:
        return getattr(self, "_active_transition_generation", 0) != getattr(
            self, "_transition_generation", 0
        )

    def capture_output_target(self):
        return self.correction_learner.capture_target()

    def recovery_texts(self) -> list[str]:
        persisted = getattr(self, "recovery_store", None)
        if persisted is not None:
            return [entry.get("text", "") for entry in persisted.load()]
        return list(self._recovery_texts)

    def retain_recovery_text(self, text: str, reason: str) -> bool:
        """Retain a partial result without risking a commit to the wrong app."""
        text = sanitize_inline_text(text)
        if text:
            self._recovery_texts.appendleft(text)
            store = getattr(self, "recovery_store", None)
            if store is not None:
                try:
                    store.add(text, reason)
                except (OSError, ValueError):
                    logger.error("恢复记录写盘失败；文字暂存内存")
                    self.show_error("恢复文字暂时无法保存，请勿退出应用")
                    return False
        self.show_error(reason)
        return bool(text)

    def type_text(self, text, error_message=None, target=_UNSET_TARGET) -> bool:
        """Commit one final transcription through a compatibility paste.

        A captured target is mandatory. Before every fallback, it must still be
        the focused accessibility element; otherwise the text is retained in a
        small in-memory recovery list and nothing is typed anywhere.
        """
        if isinstance(text, tuple):
            text, error_message = text
        if error_message:
            self.show_error(str(error_message))
            return False

        text = sanitize_inline_text(text)
        if not text:
            return False

        if target is _UNSET_TARGET:
            target = self.capture_output_target()
        if target is None:
            # An explicitly missing recording target must not be recaptured
            # after recognition: the user could now be in a different app.
            return self._retain_failed_text(text, "无法确认输入位置，文字已保留，请在恢复页复制")
        if not self._target_is_focused(target):
            return self._retain_failed_text(text, "录音期间输入焦点已改变")

        try:
            text = self.correction_learner.store.apply(text)
            snapshot = self.correction_learner.snapshot(target)
            if snapshot is None:
                return self._retain_failed_text(text, "当前输入框不可读取")
            # Each call is a distinct completed voice turn. Repeated wording is
            # legitimate; duplicate callbacks are gated by VoiceSession, not by
            # comparing content with a previous turn.
            if self._should_prepend_segment_space(target, snapshot, text):
                text = " " + text

            _current, start, _end = snapshot
            if not self._target_is_focused(target):
                return self._retain_failed_text(text, "录音期间输入焦点已改变")
            inserted = self._paste_text(target, text)
            if not inserted:
                return self._retain_failed_text(text, "当前输入框拒绝粘贴文字")

            desired_caret = start + len(text)

            self.correction_learner.observe_after_paste(text, target)
            self._last_voice_insertion = {
                "target": target,
                "end": desired_caret,
                "text": text,
            }
            logger.info("文本输入完成（%d 个字符）", len(text))
            return True
        except Exception as exc:  # noqa: BLE001
            logger.error("文本输入失败: %s", exc, exc_info=True)
            return self._retain_failed_text(text, "文字写入异常")

    def _paste_text(self, target, text: str) -> bool:
        if not self._target_is_focused(target):
            return False
        # WeChat's macOS composer consumes the pasteboard asynchronously.  A
        # longer private-paste window prevents it from seeing the clipboard
        # after it has already been restored.  Other apps keep the normal fast
        # path so the clipboard is returned almost immediately.
        settle_seconds = 0.25
        bundle_getter = getattr(self.correction_learner, "target_bundle_identifier", None)
        if bundle_getter is not None:
            try:
                bundle_id = str(bundle_getter(target) or "").casefold()
            except Exception:  # noqa: BLE001
                bundle_id = ""
            if "wechat" in bundle_id or "xwechat" in bundle_id:
                settle_seconds = 0.85
                logger.info("检测到微信输入框，延长兼容粘贴窗口至 %.2f 秒", settle_seconds)
        return paste_text_preserving_clipboard(
            text,
            settle_seconds=settle_seconds,
            before_paste_seconds=0.04,
        )

    def _retain_failed_text(self, text: str, reason: str) -> bool:
        self.retain_recovery_text(text, reason + "；请到恢复页复制文字")
        return False

    def _target_is_focused(self, target) -> bool:
        checker = getattr(self.correction_learner, "target_is_focused", None)
        return bool(checker and checker(target))

    def _should_prepend_segment_space(self, target, snapshot, text: str) -> bool:
        previous = getattr(self, "_last_voice_insertion", None)
        if not previous or snapshot is None or not text:
            return False
        current, selection_start, selection_end = snapshot
        previous_end = previous.get("end")
        previous_text = previous.get("text", "")
        if (
            previous_end is None
            or selection_start != selection_end
            or selection_start != previous_end
            or not previous_text
            or not current[:selection_start].endswith(previous_text)
        ):
            return False
        same_target = getattr(self.correction_learner, "same_target", None)
        if not same_target or not same_target(previous.get("target"), target):
            return False
        return (
            not current[selection_start - 1:selection_start].isspace()
            and not text[:1].isspace()
        )

    @staticmethod
    def _modifier_mask(names) -> int:
        from Quartz import (
            kCGEventFlagMaskAlternate,
            kCGEventFlagMaskCommand,
            kCGEventFlagMaskControl,
            kCGEventFlagMaskSecondaryFn,
            kCGEventFlagMaskShift,
        )

        masks = {
            "command": kCGEventFlagMaskCommand,
            "option": kCGEventFlagMaskAlternate,
            "control": kCGEventFlagMaskControl,
            "shift": kCGEventFlagMaskShift,
            "function": kCGEventFlagMaskSecondaryFn,
        }
        result = 0
        for name in names:
            result |= masks.get(name, 0)
        return result

    def _parse_voice_hotkey(self, value: str):
        special = {
            "fn": {"kind": "modifier", "vk": 63, "modifier": "function"},
            "left_option": {"kind": "modifier", "vk": 58, "modifier": "option"},
            "right_option": {"kind": "modifier", "vk": 61, "modifier": "option"},
            "left_command": {"kind": "modifier", "vk": 55, "modifier": "command"},
            "right_command": {"kind": "modifier", "vk": 54, "modifier": "command"},
            "left_control": {"kind": "modifier", "vk": 59, "modifier": "control"},
            "right_control": {"kind": "modifier", "vk": 62, "modifier": "control"},
        }
        if value in special:
            spec = dict(special[value])
            spec["mask"] = self._modifier_mask([spec["modifier"]])
            return spec
        if not value.startswith("keycode:"):
            return None
        try:
            parts = {}
            for part in value.split(";"):
                if ":" not in part:
                    return None
                key, raw_value = part.split(":", 1)
                if key in parts:
                    return None
                parts[key] = raw_value
            if set(parts) != {"keycode", "mods"}:
                return None
            vk = int(parts["keycode"])
            modifiers = [name for name in parts.get("mods", "").split("+") if name]
        except (KeyError, TypeError, ValueError):
            return None
        if not 0 <= vk <= 127:
            return None
        if not modifiers or len(set(modifiers)) != len(modifiers):
            return None
        if any(name not in self.VALID_REGISTERED_MODIFIERS for name in modifiers):
            return None
        normalized = f"keycode:{vk};mods:{'+'.join(modifiers)}"
        if normalized != value:
            return None
        return {
            "kind": "key",
            "vk": vk,
            "modifiers": modifiers,
            "mask": self._modifier_mask(modifiers),
        }

    @staticmethod
    def _backend_for_hotkey(spec, requested: str) -> str:
        """Resolve stale or hand-edited backend values to a usable listener."""
        if requested == "off":
            return "off"
        if spec and spec.get("kind") == "key":
            return "registered"
        return "passive"

    def _voice_hotkey_display_label(self) -> str:
        return {
            "fn": "Fn / Globe",
            "left_option": "左 Option",
            "right_option": "右 Option",
            "left_command": "左 Command",
            "right_command": "右 Command",
            "left_control": "左 Control",
            "right_control": "右 Control",
        }.get(self.voice_hotkey, self.voice_hotkey_label or "自定义快捷键")

    def _build_hotkey_suppression(self):
        spec = self._voice_hotkey_spec
        return ({spec["vk"]}, spec.get("mask", 0)) if spec else (set(), 0)

    def _set_voice_hotkey_pressed(self, pressed: bool) -> None:
        if pressed == self._voice_hotkey_pressed:
            return
        self._voice_hotkey_pressed = pressed
        mode = getattr(self, "_hotkey_mode", getattr(self, "_fn_hotkey_mode", "hold"))
        if pressed:
            if mode == "hold":
                self.start_recording()
            else:
                self.toggle_recording()
        elif mode == "hold":
            self.stop_recording()

    def _recover_hotkey_state(self) -> None:
        self._voice_hotkey_pressed = False
        if getattr(self, "_hotkey_mode", "hold") == "hold" and self.is_recording:
            self.cancel_recording()

    def _reconcile_hotkey_release(self) -> None:
        if not self._voice_hotkey_pressed:
            return
        from Quartz import CGEventSourceKeyState, kCGEventSourceStateCombinedSessionState
        spec = self._voice_hotkey_spec
        if spec and not CGEventSourceKeyState(kCGEventSourceStateCombinedSessionState, spec["vk"]):
            self._set_voice_hotkey_pressed(False)

    def _handle_configured_hotkey(self, event_type, vk, flags) -> bool:
        from Quartz import kCGEventFlagsChanged, kCGEventKeyDown, kCGEventKeyUp

        spec = self._voice_hotkey_spec
        if not spec:
            return False
        if spec["kind"] == "modifier":
            if event_type != kCGEventFlagsChanged or vk != spec["vk"]:
                return False
            self._set_voice_hotkey_pressed(bool(flags & spec["mask"]))
            return True
        if (
            event_type == kCGEventFlagsChanged
            and self._voice_hotkey_pressed
            and (flags & spec["mask"]) != spec["mask"]
        ):
            self._set_voice_hotkey_pressed(False)
            return False
        if vk != spec["vk"] or event_type not in {kCGEventKeyDown, kCGEventKeyUp}:
            return False
        if event_type == kCGEventKeyDown:
            if (flags & spec["mask"]) == spec["mask"]:
                self._set_voice_hotkey_pressed(True)
                return True
            return False
        self._set_voice_hotkey_pressed(False)
        return True

    def _shortcut_capture_active(self, *, force: bool = False) -> bool:
        now = time.monotonic()
        if not force and now - self._shortcut_capture_checked_at < 0.15:
            return self._shortcut_capture_is_active
        self._shortcut_capture_checked_at = now
        path: Path = self._shortcut_capture_lock_file
        active = False
        try:
            pid = int(path.read_text(encoding="utf-8").strip())
            os.kill(pid, 0)
            active = True
        except (OSError, ValueError):
            try:
                path.unlink(missing_ok=True)
            except OSError:
                pass
        self._shortcut_capture_is_active = active
        return active

    def _darwin_intercept(self, event_type, event):
        try:
            from Quartz import (
                CGEventGetFlags,
                CGEventGetIntegerValueField,
                CGEventTapEnable,
                kCGEventKeyDown,
                kCGEventTapDisabledByTimeout,
                kCGEventTapDisabledByUserInput,
                kCGKeyboardEventKeycode,
            )

            if event_type in {kCGEventTapDisabledByTimeout, kCGEventTapDisabledByUserInput}:
                tap = getattr(self._listener, "tap_ref", None)
                if tap is not None:
                    CGEventTapEnable(tap, True)
                    self._recover_hotkey_state()
                    now = time.time()
                    if now - self._tap_disabled_log_time > 5:
                        self._tap_disabled_log_time = now
                        logger.warning("键盘事件监听被系统暂停，已自动恢复")
                return event

            vk = CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode)
            flags = CGEventGetFlags(event)
            if self.is_recording and event_type == kCGEventKeyDown and vk == 53:
                self.cancel_recording()
                return None
            if vk in self._suppress_vks and self._shortcut_capture_active(force=True):
                return event
            if self._handle_configured_hotkey(event_type, vk, flags):
                return None
        except Exception:
            logger.exception("快捷键事件处理失败")
            return event
        return event

    def _nsevent_intercept(self, event) -> None:
        """Handle an AppKit global-monitor event without consuming it."""
        try:
            from AppKit import NSEventTypeKeyDown

            event_type = int(event.type())
            vk = int(event.keyCode())
            flags = int(event.modifierFlags())
            if self.is_recording and event_type == int(NSEventTypeKeyDown) and vk == 53:
                self.cancel_recording()
                return
            if vk in self._suppress_vks and self._shortcut_capture_active(force=True):
                return
            self._handle_configured_hotkey(event_type, vk, flags)
        except Exception as exc:  # noqa: BLE001
            logger.debug("只读快捷键事件处理失败: %s", exc)

    def start_listening(self) -> None:
        if self._hotkey_backend == "off":
            logger.info("全局快捷键已关闭；请打开设置重新选择快捷键")
            return
        if sys.platform != "darwin":
            logger.warning("全局快捷键当前仅支持 macOS")
            return

        if self._hotkey_backend == "registered":
            listener = _RegisteredHotKeyListener(
                self._voice_hotkey_spec,
                self._set_registered_hotkey_pressed,
            )
            try:
                listener.start()
                self._listener = listener
                logger.info("系统注册型录音快捷键已启用；无需输入监控权限")
            except Exception as exc:  # noqa: BLE001
                logger.error("系统注册型快捷键不可用: %s", exc)
                self.show_warning("组合键注册失败；请在设置中选择其他快捷键")
            return

        # Prefer a listen-only Quartz tap: it reports when macOS disables it
        # and can be health-checked.  Some managed Macs block event taps even
        # in listen-only mode, so retain AppKit's global monitor as a fallback.
        listener = _PassiveTapListener(
            self._darwin_intercept,
            on_recovered=self._recover_hotkey_state,
            reconcile=self._reconcile_hotkey_release,
        )
        self._listener = listener
        try:
            with listener:
                logger.info("全局录音快捷键已启用（Quartz 只读监听，可自动恢复）")
                listener.join()
            if listener.running:
                return
            raise RuntimeError("Quartz 只读监听意外结束")
        except Exception as exc:  # noqa: BLE001
            logger.warning("Quartz 快捷键监听不可用，切换到 AppKit 兼容模式: %s", exc)

        fallback = _PassiveGlobalMonitorListener(self._nsevent_intercept)
        self._listener = fallback

        def install_monitor() -> None:
            try:
                fallback.start()
                logger.info(
                    "全局录音快捷键已启用（AppKit 兼容模式，vk=%s）；录音时按 Esc 可取消",
                    sorted(self._suppress_vks),
                )
            except Exception as exc:  # noqa: BLE001
                logger.error("全局快捷键不可用: %s", exc)
                self.show_warning("请为 Voice Input 开启输入监控权限")

        # VoiceAssistant invokes this method on a worker immediately before the
        # menu-bar event loop begins. NSEvent monitors belong on that app loop.
        from PyObjCTools import AppHelper

        AppHelper.callAfter(install_monitor)

    def _set_registered_hotkey_pressed(self, pressed: bool) -> None:
        """Ignore Carbon callbacks while the settings recorder owns the keys."""
        if self._shortcut_capture_active(force=True):
            if not pressed:
                self._voice_hotkey_pressed = False
            return
        self._set_voice_hotkey_pressed(pressed)


def check_accessibility_permissions() -> None:
    logger.warning("请在系统设置 → 隐私与安全性 → 辅助功能中允许 Voice Input")
