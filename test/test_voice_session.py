import asyncio
import io
import os
import threading
import time
import unittest
from unittest.mock import patch

import numpy as np

import main as app_main


class _Recorder:
    def __init__(self):
        self.recording = False
        self.current_device = "测试麦克风"
        self.sample_rate = 16000
        self.started = threading.Event()

    def set_auto_stop_callback(self, callback):
        self.auto_stop = callback

    def set_device_disconnect_callback(self, callback):
        self.disconnect = callback

    def start_streaming_recording(self):
        self.recording = True
        self.started.set()

    def stop_streaming_recording(self, abort=False):
        self.recording = False
        return None if abort else io.BytesIO(b"test-audio")

    def reset_streaming_state(self, reason=""):
        self.recording = False

    async def stream_audio_chunks(self, **_kwargs):
        chunk = np.full(1600, 1000, dtype=np.int16).tobytes()
        yield chunk
        yield chunk
        while self.recording:
            await asyncio.sleep(0.005)


class _Archive:
    def save_audio_bytes(self, _audio):
        return None

    def save_transcription_result(self, *_args, **_kwargs):
        return None


class _Preview:
    def __init__(self):
        self.notices = []

    def show(self, **_kwargs):
        pass

    def hide(self):
        pass

    def update_level(self, _level):
        pass

    def update_text(self, _text):
        pass

    def set_processing(self):
        pass

    def show_notice(self, text, **_kwargs):
        self.notices.append(text)


class _Status:
    def __init__(self, **_kwargs):
        pass

    def update_state(self, _state, **_kwargs):
        pass

    def open_settings(self):
        pass


class _Diagnostics:
    def __init__(self):
        self.values = {}

    def update(self, **values):
        self.values.update(values)


class _Learner:
    class Store:
        @staticmethod
        def apply(text):
            return text

    store = Store()


class _Keyboard:
    def listener_ready(self):
        return True

    def __init__(self, **callbacks):
        self.callbacks = callbacks
        self.typed = []
        self.recovered = []
        self.correction_learner = _Learner()
        self.target = object()
        self.voice_hotkey_label = "右 Option"

    def _voice_hotkey_display_label(self):
        return "右 Option"

    def capture_output_target(self):
        return self.target

    def frontmost_is_settings(self):
        return False

    def mark_streaming(self):
        pass

    def reset_state(self):
        pass

    def show_warning(self, _message):
        pass

    def show_error(self, _message):
        pass

    def retain_recovery_text(self, text, reason):
        self.recovered.append((text, reason))

    def type_text(self, text, target=None):
        self.typed.append((text, target))
        return True


class _Qwen:
    def is_available(self):
        return True

    def recognition_context(self):
        return ""

    async def process_audio_stream(
        self,
        chunks,
        on_preview,
        on_final,
        on_complete,
        _on_error,
        **_kwargs,
    ):
        async for _chunk in chunks:
            on_preview("只应输入一次")
        on_final("只应输入一次")
        on_final("只应输入一次")
        on_complete()


class VoiceSessionTests(unittest.TestCase):
    def test_preparation_is_shown_before_device_start_and_scoped_to_session(self):
        assistant = self.make_assistant()
        calls = []
        assistant.floating_preview.show = lambda **kwargs: calls.append(kwargs)
        def fail_device():
            self.assertEqual(len(calls), 1)
            self.assertTrue(calls[0]["preparing"])
            self.assertTrue(calls[0]["is_relevant"]())
            return "test device unavailable"
        assistant.audio_recorder.start_streaming_recording = fail_device
        assistant.start_streaming()
        self.assertFalse(calls[0]["is_relevant"]())
        self.assertIsNone(assistant._current_session)

    def make_assistant(self):
        stack = patch.multiple(
            app_main,
            AudioRecorder=_Recorder,
            AudioArchiveManager=_Archive,
            FloatingPreviewWindow=_Preview,
            StatusBarController=_Status,
            KeyboardManager=_Keyboard,
            DiagnosticsStore=_Diagnostics,
        )
        stack.start()
        self.addCleanup(stack.stop)
        return app_main.VoiceAssistant(_Qwen())

    @staticmethod
    def wait_for_thread(assistant):
        deadline = time.time() + 2
        while time.time() < deadline:
            thread = assistant._streaming_thread
            if thread is None:
                return
            thread.join(timeout=0.05)
        raise AssertionError("streaming thread did not finish")

    def test_duplicate_final_callbacks_commit_exactly_once(self):
        with patch.dict(
            os.environ,
            {
                "AUDIO_ARCHIVE_ENABLED": "false",
                "MINIMUM_AUDIO_MS": "100",
                "MINIMUM_VOICED_AUDIO_MS": "60",
                "VOICE_RMS_THRESHOLD": "20",
            },
        ):
            assistant = self.make_assistant()
            assistant.start_streaming()
            self.assertTrue(assistant.audio_recorder.started.wait(1))
            assistant.stop_streaming()
            self.wait_for_thread(assistant)

        self.assertEqual(
            assistant.keyboard_manager.typed,
            [("只应输入一次", assistant.keyboard_manager.target)],
        )
        self.assertEqual(assistant.diagnostics.values["last_session_outcome"], "committed")
        self.assertTrue(assistant.diagnostics.values["last_session_committed"])
        self.assertGreaterEqual(assistant.diagnostics.values["last_session_preview_count"], 1)
        self.assertNotIn("只应输入一次", repr(assistant.diagnostics.values))

    def test_cancelled_session_never_commits_late_callback(self):
        with patch.dict(
            os.environ,
            {
                "AUDIO_ARCHIVE_ENABLED": "false",
                "MINIMUM_AUDIO_MS": "100",
                "MINIMUM_VOICED_AUDIO_MS": "60",
                "VOICE_RMS_THRESHOLD": "20",
            },
        ):
            assistant = self.make_assistant()
            assistant.start_streaming()
            self.assertTrue(assistant.audio_recorder.started.wait(1))
            assistant.cancel_streaming()
            self.wait_for_thread(assistant)

        self.assertEqual(assistant.keyboard_manager.typed, [])
        self.assertEqual(assistant.diagnostics.values["last_session_outcome"], "cancelled")
        self.assertFalse(assistant.diagnostics.values["last_session_committed"])

    def test_processor_setup_failure_releases_microphone(self):
        class BrokenContext(_Qwen):
            def recognition_context(self):
                raise RuntimeError("synthetic setup failure")

        assistant = self.make_assistant()
        assistant.processor = BrokenContext()
        assistant.start_streaming()
        self.wait_for_thread(assistant)
        self.assertFalse(assistant.audio_recorder.recording)
        self.assertIsNone(assistant._current_session)
        self.assertEqual(assistant.diagnostics.values["last_session_outcome"], "unexpected_error")
        self.assertTrue(assistant.floating_preview.notices)

    def test_unexpected_exception_retains_safe_preview_once(self):
        class BrokenStream(_Qwen):
            async def process_audio_stream(self, chunks, preview, *_callbacks, **_kwargs):
                for _ in range(2):
                    await anext(chunks)
                preview("原话仍然在这里")
                raise RuntimeError("synthetic stream failure")

        assistant = self.make_assistant()
        assistant.processor = BrokenStream()
        assistant.start_streaming()
        self.wait_for_thread(assistant)
        self.assertFalse(assistant.audio_recorder.recording)
        self.assertEqual([text for text, _ in assistant.keyboard_manager.recovered], ["原话仍然在这里"])
        self.assertEqual(assistant.keyboard_manager.typed, [])

    def test_completion_without_final_recovers_preview_and_releases_mic(self):
        class IncompleteStream(_Qwen):
            async def process_audio_stream(self, chunks, preview, final, complete, error, **_kwargs):
                for _ in range(2):
                    await anext(chunks)
                preview("已经识别的片段")
                complete()
                complete()

        assistant = self.make_assistant()
        assistant.processor = IncompleteStream()
        assistant.start_streaming()
        self.wait_for_thread(assistant)
        self.assertFalse(assistant.audio_recorder.recording)
        self.assertEqual(len(assistant.keyboard_manager.recovered), 1)
        self.assertEqual(assistant.diagnostics.values["last_session_outcome"], "recovered")

    def test_return_without_callbacks_does_not_leave_recording(self):
        class EmptyStream(_Qwen):
            async def process_audio_stream(self, *_args, **_kwargs):
                return

        assistant = self.make_assistant()
        assistant.processor = EmptyStream()
        assistant.start_streaming()
        self.wait_for_thread(assistant)
        self.assertFalse(assistant.audio_recorder.recording)
        self.assertIsNone(assistant._current_session)

    def test_automatic_restart_saves_preview_before_replacing_process(self):
        assistant = self.make_assistant()
        session = app_main.VoiceSession("test", None, latest_preview="重启前的原话")
        assistant._current_session = session
        def verify_saved(**_kwargs):
            self.assertEqual([x[0] for x in assistant.keyboard_manager.recovered], ["重启前的原话"])
        with patch.object(app_main, "allow_automatic_restart", return_value=True), \
             patch.object(assistant, "_restart_application", side_effect=verify_saved) as restart:
            assistant._recover_runtime("设备超时")
        restart.assert_called_once()
        self.assertTrue(session.cancelled)

    def test_cancelled_or_already_emitted_text_is_not_recovered_on_restart(self):
        assistant = self.make_assistant()
        with patch.object(app_main, "allow_automatic_restart", return_value=False):
            for flags in ({"cancelled": True}, {"final_emitted": True}):
                assistant._current_session = app_main.VoiceSession(
                    "test", None, latest_preview="不应重复保存", **flags
                )
                assistant._recover_runtime("设备超时")
        self.assertEqual(assistant.keyboard_manager.recovered, [])

    def test_old_session_cleanup_does_not_stop_new_microphone(self):
        assistant = self.make_assistant()
        old = app_main.VoiceSession("old", None)
        current = app_main.VoiceSession("current", None)
        assistant._current_session = current
        assistant.audio_recorder.recording = True
        assistant._finish_session(old)
        self.assertTrue(assistant.audio_recorder.recording)
        self.assertIs(assistant._current_session, current)

    def test_user_cancel_never_retains_partial_text(self):
        assistant = self.make_assistant()
        session = app_main.VoiceSession("test", None, latest_preview="取消后不应保留")
        assistant._current_session = session
        assistant.audio_recorder.recording = True
        assistant.cancel_streaming()
        self.assertFalse(assistant.audio_recorder.recording)
        self.assertEqual(assistant.keyboard_manager.recovered, [])
        self.assertEqual(assistant.floating_preview.notices, [])

    def test_archive_failure_does_not_discard_final_text(self):
        assistant = self.make_assistant()
        with patch.object(assistant, "_save_transcription_cache", side_effect=OSError("disk full")):
            assistant.start_streaming()
            assistant.stop_streaming()
            self.wait_for_thread(assistant)
        self.assertEqual(len(assistant.keyboard_manager.typed), 1)


if __name__ == "__main__":
    unittest.main()
