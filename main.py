"""Qwen/Doubao macOS system-wide voice input application."""

from __future__ import annotations

import asyncio
import io
import os
import sys
import threading
import time
import uuid
from contextlib import aclosing
from dataclasses import dataclass, field
from typing import Any, Optional

import numpy as np
from dotenv import load_dotenv

# Use macOS Keychain (and the user's full trust store, including any
# corporate root CAs imported by IT) for TLS verification.  Without this,
# Python's default openssl CA bundle cannot validate certs re-signed by
# enterprise SSL inspection (Zscaler, Fortinet, ...), and every WebSocket
# connect to Qwen/Doubao fails with CERTIFICATE_VERIFY_FAILED.  Inject
# before any aiohttp/ssl code runs.
try:
    import truststore

    truststore.inject_into_ssl()
except Exception as _truststore_err:  # noqa: BLE001
    # truststore missing or unavailable — fall back to the default openssl
    # bundle.  Home users without corporate proxies won't notice.
    pass

from src.runtime_paths import DATA_DIR, ENV_FILE, IS_FROZEN, ensure_runtime_layout

ensure_runtime_layout()
load_dotenv(ENV_FILE, override=True)

from src.audio.archive import AudioArchiveManager
from src.audio.recorder import AudioRecorder
from src.diagnostics import DiagnosticsStore
from src.runtime_health import RuntimeHealth, allow_automatic_restart
from src.keyboard.inputState import InputState
from src.keyboard.listener import KeyboardManager, check_accessibility_permissions
from src.login_item import resume_supervisor_for_manual_launch, sync_login_item
from src.permissions import PermissionMonitor
from src.text_processing import (
    clean_spoken_disfluencies,
    format_transcription_text,
    has_sufficient_speech,
    is_context_echo,
    sanitize_inline_text,
)
from src.transcription.doubao_streaming import DoubaoStreamingProcessor
from src.transcription.qwen_streaming import QwenStreamingProcessor
from src.ui.floating_preview import FloatingPreviewWindow
from src.ui.status_bar import StatusBarController
from src.utils.logger import logger


__version__ = "0.2.3"
__author__ = "Mor-Li"
__description__ = "Qwen and Doubao realtime voice input for macOS"


def _should_open_settings_on_start(
    *,
    processor_present: bool,
    is_frozen: bool,
    arguments: list[str],
    restarting: bool,
) -> bool:
    """Only a deliberate foreground launch may open the settings window."""
    if "--show-settings-after-restart" in arguments:
        return True
    if "--background-login" in arguments or restarting:
        return False
    return not processor_present or is_frozen


@dataclass
class VoiceSession:
    """All mutable state belonging to exactly one push-to-talk utterance."""

    session_id: str
    output_target: object
    cancelled: bool = False
    final_emitted: bool = False
    completed: bool = False
    latest_preview: str = ""
    archive_path: Optional[str] = None
    started_at: float = field(default_factory=time.monotonic)
    preview_count: int = 0
    first_preview_ms: Optional[int] = None
    total_audio_ms: int = 0
    voiced_audio_ms: int = 0
    final_committed: bool = False
    recovery_saved: bool = False
    outcome: str = "recording"
    lock: threading.RLock = field(default_factory=threading.RLock, repr=False)


def check_microphone_permissions() -> None:
    logger.warning("请在系统设置 → 隐私与安全性 → 麦克风中允许 Veyqa Voice")


class VoiceAssistant:
    """Coordinate one cloud-ASR streaming session at a time."""

    def __init__(self, processor: Optional[Any], service: str = "qwen") -> None:
        self.audio_recorder = AudioRecorder()
        self.audio_archive = AudioArchiveManager()
        self.diagnostics = DiagnosticsStore()
        login_enabled = os.getenv("LAUNCH_AT_LOGIN", "true").lower() == "true"
        login_result = sync_login_item(login_enabled)
        self.processor = processor
        self.service = service if service in {"qwen", "doubao"} else "qwen"
        self.engine_label = "豆包" if self.service == "doubao" else "千问"
        self.punctuation_mode = os.getenv("PUNCTUATION_MODE", "auto").lower()
        self.archive_enabled = os.getenv("AUDIO_ARCHIVE_ENABLED", "false").lower() == "true"

        self._current_state = InputState.IDLE
        self._state_since = time.monotonic()
        self._session_lock = threading.RLock()
        self._current_session: Optional[VoiceSession] = None
        self._streaming_thread: Optional[threading.Thread] = None

        self.floating_preview = FloatingPreviewWindow()

        configured = bool(processor and processor.is_available())
        self.keyboard_manager = KeyboardManager(
            on_record_start=self.start_streaming if configured else self._start_unconfigured,
            on_record_stop=self.stop_streaming if configured else self._stop_unconfigured,
            on_record_cancel=self.cancel_streaming,
            on_state_change=self._on_state_change,
        )
        self.permission_monitor = PermissionMonitor(
            version=__version__,
            hotkey_backend=getattr(self.keyboard_manager, "_hotkey_backend", "passive"),
        )
        self.status_controller = StatusBarController(
            on_restart=self._restart_application,
            open_settings_on_start=_should_open_settings_on_start(
                processor_present=bool(processor),
                is_frozen=IS_FROZEN,
                arguments=sys.argv,
                restarting=os.getenv("VOICE_INPUT_RESTARTING") == "true",
            ),
        )
        self.audio_recorder.set_auto_stop_callback(self._handle_auto_stop)
        self.audio_recorder.set_device_disconnect_callback(self._handle_device_disconnect)
        self.diagnostics.update(
            version=__version__,
            engine=self.service,
            state=InputState.IDLE.name,
            microphone=self.audio_recorder.current_device or "未知",
            sample_rate=self.audio_recorder.sample_rate,
            hotkey=self.keyboard_manager.voice_hotkey_label
            or self.keyboard_manager._voice_hotkey_display_label(),
            hotkey_backend=getattr(self.keyboard_manager, "_hotkey_backend", "passive"),
            archive_enabled=self.archive_enabled,
            last_error=login_result.error,
            login_item_status=login_result.status,
            login_item_error=login_result.error,
            recovery_status="后台服务就绪",
        )
        self._notify_status()
        self.health = RuntimeHealth(self._health_fault, self._recover_runtime)

        logger.info(
            "语音引擎：%s；标点模式：%s；本地录音存档：%s",
            self.engine_label,
            self.punctuation_mode,
            "开启" if self.archive_enabled else "关闭",
        )

    @staticmethod
    def _restart_application(*, show_settings: bool = True) -> None:
        (DATA_DIR / ".restart-request").unlink(missing_ok=True)
        logger.info("设置已保存，正在重启语音输入服务")
        os.environ["VOICE_INPUT_RESTARTING"] = "true"
        arguments = [
            argument
            for argument in sys.argv[1:]
            if argument not in {"--show-settings-after-restart", "--background-login"}
        ]
        os.execv(
            sys.executable,
            [sys.executable, "--show-settings-after-restart" if show_settings else "--background-login", *arguments],
        )

    def _health_fault(self, now: float) -> str:
        started = getattr(self.keyboard_manager, "_transition_started_at", None)
        if started is not None and now - started > 10:
            return "录音设备操作超时"
        if self._current_state == InputState.PROCESSING and now - self._state_since > 45:
            return "识别结束超时"
        listener = getattr(self.keyboard_manager, "_listener", None)
        polled = getattr(listener, "last_poll_at", None)
        if polled is not None and now - polled > 15:
            return "快捷键监听循环无响应"
        return ""

    def _recover_runtime(self, reason: str) -> None:
        session = self._current_session
        if session is not None:
            with session.lock:
                self._retain_session_preview(session, reason)
                session.cancelled = True
        allowed = allow_automatic_restart()
        self.diagnostics.update(last_error=reason, recovery_status=(
            "正在自动恢复" if allowed else "恢复次数过多，请检查音频设备后重新启动"
        ))
        logger.warning("后台功能异常：%s；自动恢复=%s", reason, allowed)
        if allowed:
            # exec resets stuck native audio state and preserves PID/settings helper.
            # A separate native supervisor covers a blocked GIL or UI thread.
            self._restart_application(show_settings=False)

    def _tick_health(self) -> None:
        from PyObjCTools import AppHelper
        self.health.tick_main()
        if (DATA_DIR / ".restart-request").exists():
            self._restart_application()
            return
        AppHelper.callLater(2, self._tick_health)

    def _on_state_change(self, state: InputState) -> None:
        self._current_state = state
        self._state_since = time.monotonic()
        self.diagnostics.update(state=state.name)
        self._notify_status()

    def _notify_status(self) -> None:
        try:
            self.status_controller.update_state(self._current_state)
        except Exception as exc:  # noqa: BLE001
            logger.debug("更新状态栏失败: %s", exc)

    def _start_unconfigured(self) -> None:
        self.keyboard_manager.show_warning(f"请先在设置中填写{self.engine_label}凭证")
        self.status_controller.open_settings()

    def _stop_unconfigured(self) -> None:
        self.keyboard_manager.reset_state()

    def _is_current(self, session: VoiceSession) -> bool:
        with self._session_lock:
            return self._current_session is session

    def _finish_session(self, session: VoiceSession, *, notice: str = "") -> None:
        # Serialize ownership and cleanup together: an old callback must never
        # close the next session's microphone or reset its keyboard state.
        with self._session_lock:
            if self._current_session is not session:
                return
            with session.lock:
                if session.completed:
                    return
                session.completed = True
            try:
                self.audio_recorder.reset_streaming_state(reason="会话结束")
            finally:
                self._current_session = None
                self._finished_session_id = session.session_id
                self.floating_preview.hide()
                self.keyboard_manager.reset_state()
                if notice:
                    session_id = session.session_id
                    self.floating_preview.show_notice(notice, is_relevant=lambda: (
                        self._current_session is None
                        and self._finished_session_id == session_id
                        and not getattr(self.keyboard_manager, "is_recording", False)
                    ))

    def _retain_session_preview(self, session: VoiceSession, reason: str) -> bool:
        # Previews only enter this field after the speech/context safety gates.
        # Never save cancelled content or duplicate a result handled by type_text.
        with session.lock:
            if (session.cancelled or session.final_emitted or session.recovery_saved
                    or not session.latest_preview):
                return False
            try:
                saved = self.keyboard_manager.retain_recovery_text(
                    session.latest_preview, reason + "；已识别片段可在恢复页复制"
                )
                session.recovery_saved = saved is not False
            except Exception:
                logger.error("无法保存本轮恢复文字")
            return session.recovery_saved

    def _fail_session(self, session: VoiceSession, reason: str, outcome: str) -> None:
        if not self._is_current(session) or session.cancelled:
            return
        notice = reason
        try:
            if self._retain_session_preview(session, reason):
                notice = "已保留识别片段，请打开恢复页复制"
            else:
                self.keyboard_manager.show_error(reason)
            self.diagnostics.update(last_error=reason)
            self._record_session_diagnostics(session, outcome)
        finally:
            self._finish_session(session, notice=notice)

    def _record_session_diagnostics(self, session: VoiceSession, outcome: str) -> None:
        """Persist non-content quality signals for troubleshooting.

        Never store the recognized text, context, target application, API Key,
        or a session identifier. These coarse metrics are enough to distinguish
        microphone gating, network preview, and final insertion failures.
        """
        with session.lock:
            session.outcome = outcome
            values = {
                "last_session_outcome": outcome,
                "last_session_audio_ms": int(session.total_audio_ms),
                "last_session_voiced_ms": int(session.voiced_audio_ms),
                "last_session_preview_count": int(session.preview_count),
                "last_session_first_preview_ms": session.first_preview_ms,
                "last_session_committed": bool(session.final_committed),
            }
        self.diagnostics.update(**values)

    @staticmethod
    def _buffer_to_bytes(audio_buffer: Optional[io.BytesIO]) -> Optional[bytes]:
        if audio_buffer is None:
            return None
        try:
            audio_buffer.seek(0)
            return audio_buffer.read()
        finally:
            try:
                audio_buffer.close()
            except Exception:
                pass

    def _archive_buffer(self, session: VoiceSession, audio_buffer: Optional[io.BytesIO]) -> None:
        audio_bytes = self._buffer_to_bytes(audio_buffer)
        if not audio_bytes or not self.archive_enabled or session.cancelled:
            return
        session.archive_path = self.audio_archive.save_audio_bytes(audio_bytes)

    def _save_transcription_cache(self, session: VoiceSession, text: str) -> None:
        if not self.archive_enabled or not session.archive_path or not text:
            return
        self.audio_archive.save_transcription_result(
            session.archive_path,
            text,
            service=self.service,
            model=getattr(self.processor, "model_name", self.service),
            mode="transcriptions",
        )

    def start_streaming(self) -> None:
        """Start a new ASR session after the event-tap transition is serialized."""
        if not self.processor or not self.processor.is_available():
            self._start_unconfigured()
            return
        with self._session_lock:
            # Cancellation can arrive after dequeue but before the session exists.
            # The same lock is used by cancel_streaming, closing that race too.
            if getattr(self.keyboard_manager, "transition_was_cancelled", lambda: False)():
                return
            if self._current_session is not None:
                logger.warning("上一轮语音会话尚未结束，已忽略重复启动")
                self.keyboard_manager.reset_state()
                return
            if self._streaming_thread and self._streaming_thread.is_alive():
                logger.warning("上一轮%s连接仍在清理，已忽略本次启动", self.engine_label)
                self.keyboard_manager.reset_state()
                return

            target = self.keyboard_manager.capture_output_target()
            session = VoiceSession(uuid.uuid4().hex, target)
            self._current_session = session

        if target is None:
            logger.warning("未能锁定当前输入框；识别结果不会写入其他窗口")
        else:
            logger.info("已锁定本轮语音输出目标")

        try:
            error = self.audio_recorder.start_streaming_recording()
            if error:
                raise RuntimeError(str(error))
        except Exception as exc:  # noqa: BLE001
            logger.error("启动录音失败: %s", exc)
            self.diagnostics.update(last_error="麦克风启动失败")
            self.keyboard_manager.show_error("麦克风启动失败")
            self._finish_session(session, notice="麦克风启动失败，请检查设备与权限")
            return

        if session.cancelled or not self._is_current(session):
            self.audio_recorder.stop_streaming_recording(abort=True)
            self._finish_session(session)
            return

        self.keyboard_manager.mark_streaming()
        self.diagnostics.update(
            microphone=self.audio_recorder.current_device or "未知",
            sample_rate=self.audio_recorder.sample_rate,
            target_locked=target is not None,
            last_error="",
        )
        thread = threading.Thread(
            target=self._streaming_thread_main,
            args=(session,),
            name=f"{self.service}-stream-{session.session_id[:8]}",
            daemon=True,
        )
        self._streaming_thread = thread
        thread.start()

    def _streaming_thread_main(self, session: VoiceSession) -> None:
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)
        try:
            loop.run_until_complete(self._run_streaming(session))
        except Exception as exc:  # noqa: BLE001
            logger.error("%s流式线程异常: %s", self.engine_label, exc, exc_info=True)
            self._fail_session(session, f"{self.engine_label}识别意外中断", "unexpected_error")
        else:
            # Providers must complete explicitly; a silent return is not success.
            self._fail_session(session, "识别未返回完整结果，请重试", "incomplete")
        finally:
            loop.run_until_complete(loop.shutdown_asyncgens())
            loop.close()
            if self._streaming_thread is threading.current_thread():
                self._streaming_thread = None

    async def _run_streaming(self, session: VoiceSession) -> None:
        """Stream audio, preview cumulative text, and commit one final result."""
        if not self._is_current(session) or session.cancelled:
            return

        self.floating_preview.show()
        self.floating_preview.update_text("正在听写…")
        total_audio_ms = 0.0
        voiced_audio_ms = 0.0
        voice_gate_open = False
        stream_failed = False

        def env_float(name: str, default: float, minimum: float) -> float:
            try:
                return max(minimum, float(os.getenv(name, str(default))))
            except ValueError:
                return default

        voice_rms_threshold = env_float("VOICE_RMS_THRESHOLD", 60.0, 20.0)
        minimum_voiced_ms = env_float("MINIMUM_VOICED_AUDIO_MS", 100.0, 60.0)
        minimum_total_ms = env_float("MINIMUM_AUDIO_MS", 180.0, 100.0)

        # Build a fresh processor so a cancelled WebSocket cannot leak callbacks
        # or internal connection state into the next utterance.
        processor = type(self.processor)()
        recognition_context = processor.recognition_context()

        def active() -> bool:
            return self._is_current(session) and not session.cancelled

        def prepare_text(text: str) -> str:
            formatted = format_transcription_text(text, self.punctuation_mode)
            cleaned = clean_spoken_disfluencies(formatted)
            return sanitize_inline_text(cleaned)

        def output_is_safe(text: str) -> bool:
            if not has_sufficient_speech(
                total_audio_ms,
                voiced_audio_ms,
                minimum_total_ms,
                minimum_voiced_ms,
            ):
                logger.warning(
                    "已拦截极短或静音结果（音频 %.0fms，语音 %.0fms）",
                    total_audio_ms,
                    voiced_audio_ms,
                )
                return False
            if is_context_echo(text, recognition_context):
                logger.error("已拦截疑似个性化上下文回显")
                return False
            return True

        def emit_final(text: str) -> bool:
            if not active() or not output_is_safe(text):
                return False
            text = prepare_text(text)
            if not text:
                return False
            with session.lock:
                if session.final_emitted or session.cancelled:
                    logger.warning("已忽略本轮重复的最终回调")
                    return False
                session.final_emitted = True
            try:
                self._save_transcription_cache(session, text)
            except Exception:
                # Optional archives must not prevent the actual dictation.
                logger.warning("本地转写存档失败，继续写入文字")
            inserted = self.keyboard_manager.type_text(
                text,
                target=session.output_target,
            )
            with session.lock:
                session.final_committed = inserted
            logger.info(
                "本轮最终结果处理完成（%d 个字符，写入=%s）",
                len(text),
                inserted,
            )
            return inserted

        def on_preview_text(text: str) -> None:
            if not active() or not voice_gate_open:
                return
            if is_context_echo(text, recognition_context):
                return
            preview = prepare_text(text)
            if not preview:
                return
            with session.lock:
                session.latest_preview = preview
                session.preview_count += 1
                if session.first_preview_ms is None:
                    session.first_preview_ms = int(
                        (time.monotonic() - session.started_at) * 1000
                    )
            self.floating_preview.update_text(preview)

        def on_final_text(text: str) -> None:
            if not stream_failed:
                emit_final(text)

        def on_complete() -> None:
            if active():
                logger.info("%s流式识别完成", self.engine_label)
                with session.lock:
                    committed = session.final_committed
                    had_preview = session.preview_count > 0
                recovered = self._retain_session_preview(session, "未收到完整识别结果")
                outcome = "committed" if committed else "recovered" if recovered else (
                    "no_safe_result" if not had_preview else "not_committed")
                notice = "" if committed else (
                    "未检测到可用语音，请检查麦克风" if outcome == "no_safe_result"
                    else "文字未写入，请打开恢复页查看")
                try:
                    if outcome == "no_safe_result":
                        self.keyboard_manager.show_warning("未检测到可用语音，请检查麦克风后重试")
                    self._record_session_diagnostics(session, outcome)
                finally:
                    self._finish_session(session, notice=notice)

        def on_error(error: str) -> None:
            nonlocal stream_failed
            if stream_failed or not active():
                return
            stream_failed = True
            logger.error("%s流式识别失败: %s", self.engine_label, error)
            self._fail_session(session, f"{self.engine_label}连接中断，请检查网络或服务配置", "network_error")

        async def audio_chunks_with_levels():
            nonlocal total_audio_ms, voiced_audio_ms, voice_gate_open
            async with aclosing(self.audio_recorder.stream_audio_chunks(
                chunk_duration_ms=100,
                target_sample_rate=16000,
            )) as chunks:
                async for chunk in chunks:
                    if not active():
                        return
                    samples = np.frombuffer(chunk, dtype=np.int16)
                    if samples.size:
                        rms = float(np.sqrt(np.mean(samples.astype(np.float32) ** 2)))
                        chunk_ms = samples.size / 16.0
                        total_audio_ms += chunk_ms
                        if rms >= voice_rms_threshold:
                            voiced_audio_ms += chunk_ms
                        voice_gate_open = has_sufficient_speech(
                            total_audio_ms,
                            voiced_audio_ms,
                            minimum_total_ms,
                            minimum_voiced_ms,
                        )
                        with session.lock:
                            session.total_audio_ms = int(total_audio_ms)
                            session.voiced_audio_ms = int(voiced_audio_ms)
                        normalized = max(0.0, min(1.0, (rms - 35.0) / 850.0))
                        self.floating_preview.update_level(normalized ** 0.5 if normalized else 0.0)
                    yield chunk

        async with aclosing(audio_chunks_with_levels()) as chunks:
            await processor.process_audio_stream(
                chunks, on_preview_text, on_final_text, on_complete, on_error,
                sample_rate=16000,
            )

    def stop_streaming(self) -> None:
        with self._session_lock:
            session = self._current_session
        if session is None or session.cancelled:
            self.keyboard_manager.reset_state()
            return
        logger.info("停止本轮%s录音", self.engine_label)
        self.floating_preview.update_text("正在完成识别…")
        audio = self.audio_recorder.stop_streaming_recording()
        self._archive_buffer(session, audio)

    def cancel_streaming(self) -> None:
        with self._session_lock:
            session = self._current_session
        if session is None:
            self.audio_recorder.stop_streaming_recording(abort=True)
            return
        with session.lock:
            session.cancelled = True
        self.audio_recorder.stop_streaming_recording(abort=True)
        logger.info("本轮语音输入已取消，不会提交识别结果")
        self._record_session_diagnostics(session, "cancelled")
        self._finish_session(session)

    def _handle_auto_stop(self) -> None:
        logger.warning("录音达到最大时长，已自动结束并提交现有音频")
        self.stop_streaming()

    def _handle_device_disconnect(self) -> None:
        logger.warning("录音设备断开，正在提交已录制音频")
        self.diagnostics.update(last_error="录音设备已断开")
        self.stop_streaming()

    def run(self) -> None:
        logger.info("语音输入已启动 (v%s)", __version__)
        self.permission_monitor.start()
        self.health.start()
        from PyObjCTools import AppHelper
        AppHelper.callAfter(self._tick_health)
        threading.Thread(
            target=self.keyboard_manager.start_listening,
            name="keyboard-listener",
            daemon=True,
        ).start()
        # Release verification only: never activate against normal user storage.
        if (os.getenv("VEYQA_HEALTH_SELF_TEST") == "true"
                and os.getenv("VOICE_INPUT_DATA_DIR")
                and os.getenv("VOICE_INPUT_DISABLE_LOGIN_SYNC") == "true"):
            marker = DATA_DIR / ".health-fault-injected"
            if not marker.exists():
                marker.write_text("once", encoding="utf-8")
                self.keyboard_manager.on_record_start = lambda: threading.Event().wait(300)
                self.keyboard_manager.start_recording()
        self.status_controller.start()


def main() -> None:
    try:
        resume_supervisor_for_manual_launch(sys.argv[1:])
        service = os.getenv("TRANSCRIPTION_SERVICE", "qwen").strip().lower()
        if service == "doubao":
            processor = DoubaoStreamingProcessor()
        else:
            service = "qwen"
            processor = QwenStreamingProcessor()
        assistant = VoiceAssistant(
            processor if processor.is_available() else None,
            service=service,
        )
        assistant.run()
    except Exception as exc:  # noqa: BLE001
        message = str(exc)
        if "Input event monitoring will not be possible" in message:
            check_accessibility_permissions()
        elif "无法访问音频设备" in message:
            check_microphone_permissions()
        else:
            logger.error("应用启动失败: %s", message, exc_info=True)
        sys.exit(1)


if __name__ == "__main__":
    main()
