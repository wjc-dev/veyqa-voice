import unittest
import sys
from types import SimpleNamespace
from collections import deque
from unittest.mock import patch

from src.keyboard.inputState import InputState
from src.keyboard.listener import KeyboardManager
from src.correction_learning import CorrectionLearner


class _Store:
    @staticmethod
    def apply(text):
        return text


class _CorrectionLearner:
    def __init__(
        self,
        *,
        ax_succeeds=True,
        ax_changes_text=True,
        openai_target=False,
        event_input=False,
        focused=True,
    ):
        self.store = _Store()
        self.target = object()
        self.ax_succeeds = ax_succeeds
        self.ax_changes_text = ax_changes_text
        self.openai_target = openai_target
        self.event_input = event_input
        self.event_calls = []
        self.focused = focused
        self.current_text = "已有文字"
        self.selection = (4, 4)
        self.replacements = []
        self.value_replacements = []
        self.observed = []
        self.capture_count = 0

    def capture_target(self):
        self.capture_count += 1
        return self.target

    def target_is_focused(self, target):
        return target is self.target and self.focused

    def snapshot(self, target):
        if target is not self.target:
            return None
        return (self.current_text, *self.selection)

    def replace_text_range(self, target, start, length, text):
        self.replacements.append((target, start, length, text))
        if self.ax_succeeds and self.ax_changes_text:
            self.current_text = (
                self.current_text[:start] + text + self.current_text[start + length:]
            )
            caret = start + len(text)
            self.selection = (caret, caret)
        return self.ax_succeeds

    def replace_value_range(self, target, start, length, text):
        self.value_replacements.append((target, start, length, text))
        if self.ax_succeeds and self.ax_changes_text:
            self.current_text = (
                self.current_text[:start] + text + self.current_text[start + length:]
            )
            caret = start + len(text)
            self.selection = (caret, caret)
        return self.ax_succeeds

    def set_caret(self, target, index):
        if target is not self.target or index > len(self.current_text):
            return False
        self.selection = (index, index)
        return True

    def _is_openai_desktop_target(self, target):
        return target is self.target and self.openai_target

    def prefers_value_write(self, target):
        return target is self.target and self.openai_target

    def uses_event_text_input(self, target):
        return target is self.target and self.event_input

    def insert_text_event(self, target, text):
        self.event_calls.append((target, text))
        if target is not self.target:
            return False
        start, end = self.selection
        self.current_text = self.current_text[:start] + text + self.current_text[end:]
        caret = start + len(text)
        self.selection = (caret, caret)
        return True

    @staticmethod
    def same_target(first, second):
        return first is second

    def observe_after_paste(self, text, target):
        self.observed.append((text, target))


class FinalTextInsertionTests(unittest.TestCase):
    def test_synthetic_carrier_key_writer_does_not_exist(self):
        self.assertFalse(hasattr(KeyboardManager, "_insert_unicode_text"))

    @staticmethod
    def make_manager(
        *,
        ax_succeeds=True,
        ax_changes_text=True,
        openai_target=False,
        event_input=False,
        focused=True,
    ):
        manager = KeyboardManager.__new__(KeyboardManager)
        manager.correction_learner = _CorrectionLearner(
            ax_succeeds=ax_succeeds,
            ax_changes_text=ax_changes_text,
            openai_target=openai_target,
            event_input=event_input,
            focused=focused,
        )
        manager._state = InputState.IDLE
        manager._recovery_texts = deque(maxlen=5)
        manager._last_voice_insertion = None
        manager._state_messages = {InputState.IDLE: ""}
        manager.state_symbol_enabled = False
        manager.on_state_change = None
        manager.processing_text = None
        manager.error_message = None
        manager.warning_message = None
        manager.temp_text_length = 0
        manager.pasted_texts = []

        def paste_text(target, text):
            learner = manager.correction_learner
            manager.pasted_texts.append(text)
            if not learner.ax_succeeds or target is not learner.target:
                return False
            start, end = learner.selection
            learner.current_text = learner.current_text[:start] + text + learner.current_text[end:]
            caret = start + len(text)
            learner.selection = (caret, caret)
            return True

        manager._paste_text = paste_text
        manager.show_error = lambda message: (_ for _ in ()).throw(AssertionError(message))
        return manager

    def test_focus_change_never_types_into_the_new_foreground_app(self):
        manager = self.make_manager(focused=False)
        manager.show_error = lambda _message: None
        inserted = manager.type_text(
            "绝不能写到别的窗口",
            target=manager.correction_learner.target,
        )

        self.assertFalse(inserted)
        self.assertEqual(manager.recovery_texts(), ["绝不能写到别的窗口"])

    def test_ax_inserts_the_complete_final_text_once(self):
        manager = self.make_manager()
        with patch("src.keyboard.listener.time.sleep", return_value=None):
            manager.type_text("开头和结尾都保留", None)

        self.assertEqual(manager.pasted_texts, ["开头和结尾都保留"])
        self.assertEqual(
            manager.correction_learner.observed,
            [("开头和结尾都保留", manager.correction_learner.target)],
        )
        self.assertEqual(
            manager.correction_learner.selection,
            (4 + len("开头和结尾都保留"),) * 2,
        )

    def test_failed_ax_write_never_emits_raw_unicode_carrier_key(self):
        manager = self.make_manager(ax_succeeds=False)
        manager.show_error = lambda _message: None
        with patch("src.keyboard.listener.time.sleep", return_value=None):
            inserted = manager.type_text("完整结果", None)

        self.assertFalse(inserted)
        self.assertEqual(manager.recovery_texts(), ["完整结果"])

    def test_uses_target_captured_before_preview(self):
        manager = self.make_manager()
        early_target = manager.correction_learner.target
        with patch("src.keyboard.listener.time.sleep", return_value=None):
            manager.type_text("锁定原光标", None, target=early_target)

        self.assertEqual(manager.correction_learner.capture_count, 0)

    def test_failed_compatibility_paste_is_retained_for_recovery(self):
        manager = self.make_manager()
        manager.show_error = lambda _message: None
        manager._paste_text = lambda _target, _text: False

        with patch("src.keyboard.listener.time.sleep", return_value=None):
            inserted = manager.type_text("不能假成功", None)

        self.assertFalse(inserted)
        self.assertEqual(manager.recovery_texts(), ["不能假成功"])

    def test_openai_editor_uses_ax_value_without_unicode_carrier_key(self):
        manager = self.make_manager(openai_target=True)

        with patch("src.keyboard.listener.time.sleep", return_value=None):
            inserted = manager.type_text("不会先跳到开头", None)

        self.assertTrue(inserted)
        self.assertEqual(manager.pasted_texts, ["不会先跳到开头"])
        self.assertEqual(
            manager.correction_learner.selection,
            (4 + len("不会先跳到开头"),) * 2,
        )

    def test_electron_editor_uses_real_text_event_instead_of_ax_value(self):
        manager = self.make_manager(event_input=True)

        with patch("src.keyboard.listener.time.sleep", return_value=None):
            inserted = manager.type_text("立即可以编辑", None)

        self.assertTrue(inserted)
        self.assertEqual(manager.pasted_texts, ["立即可以编辑"])
        self.assertEqual(manager.correction_learner.event_calls, [])

    def test_line_breaks_are_spaces_not_submit_keys(self):
        manager = self.make_manager()
        with patch("src.keyboard.listener.time.sleep", return_value=None):
            manager.type_text("第一段\n第二段\r\n结尾", None)
        self.assertEqual(
            manager.pasted_texts[-1],
            "第一段 第二段 结尾",
        )

    def test_consecutive_voice_segments_get_one_separator(self):
        manager = self.make_manager()
        with patch("src.keyboard.listener.time.sleep", return_value=None):
            manager.type_text("第一段", None)
            manager.type_text("第二段", None)

        self.assertEqual(manager.correction_learner.current_text, "已有文字第一段 第二段")
        self.assertEqual(manager.pasted_texts[-1], " 第二段")

    def test_recent_exact_duplicate_voice_segment_is_not_inserted_twice(self):
        manager = self.make_manager()
        with patch("src.keyboard.listener.time.sleep", return_value=None):
            self.assertTrue(manager.type_text("你现在是可以用的吗", None))
            self.assertTrue(manager.type_text("你现在是可以用的吗", None))

        self.assertEqual(manager.correction_learner.current_text, "已有文字你现在是可以用的吗")
        self.assertEqual(len(manager.pasted_texts), 1)

    def test_recent_overlapping_voice_segment_only_appends_new_suffix(self):
        manager = self.make_manager()
        with patch("src.keyboard.listener.time.sleep", return_value=None):
            manager.type_text("你现在可以", None)
            manager.type_text("现在可以继续吗", None)

        self.assertEqual(manager.correction_learner.current_text, "已有文字你现在可以 继续吗")
        self.assertEqual(manager.pasted_texts[-1], " 继续吗")

    def test_moving_caret_does_not_add_voice_segment_separator(self):
        manager = self.make_manager()
        with patch("src.keyboard.listener.time.sleep", return_value=None):
            manager.type_text("第一段", None)
            manager.correction_learner.selection = (0, 0)
            manager.type_text("插入开头", None)

        self.assertEqual(manager.pasted_texts[-1], "插入开头")

    def test_visual_placeholder_is_not_preserved_as_user_text(self):
        self.assertEqual(
            CorrectionLearner._remove_visual_placeholder(
                "Do anything", "Do anything", 0, 0
            ),
            ("", 0, 0),
        )

    def test_visual_placeholder_with_caret_at_visual_end_is_empty(self):
        self.assertEqual(
            CorrectionLearner._remove_visual_placeholder(
                "随心输入", "随心输入", len("随心输入"), 0
            ),
            ("", 0, 0),
        )

    def test_zero_real_character_count_identifies_localized_placeholder(self):
        learner = CorrectionLearner.__new__(CorrectionLearner)
        target = type("Target", (), {"element": object()})()

        def copy_attribute(_element, attribute, _value):
            if attribute == "AXNumberOfCharacters":
                return 0, 0
            return 1, None

        with patch(
            "ApplicationServices.AXUIElementCopyAttributeValue",
            side_effect=copy_attribute,
        ):
            self.assertEqual(
                learner._visual_placeholder(target, "任意语言占位", 0, 0),
                "任意语言占位",
            )

    def test_qoder_visual_prompt_is_not_treated_as_real_text(self):
        learner = CorrectionLearner.__new__(CorrectionLearner)
        target = type("Target", (), {"element": object()})()
        prompt = "规划与编程，@ 添加上下文，/ 使用命令"

        with (
            patch(
                "ApplicationServices.AXUIElementCopyAttributeValue",
                return_value=(1, None),
            ),
            patch.object(learner, "_is_openai_desktop_target", return_value=False),
            patch.object(learner, "_is_qoder_desktop_target", return_value=True),
        ):
            self.assertEqual(
                learner._visual_placeholder(target, prompt, len(prompt), len(prompt)),
                prompt,
            )

    def test_qoder_follow_up_prompt_is_not_treated_as_real_text(self):
        learner = CorrectionLearner.__new__(CorrectionLearner)
        target = type("Target", (), {"element": object()})()
        prompt = "追加需求或提问"

        with (
            patch(
                "ApplicationServices.AXUIElementCopyAttributeValue",
                return_value=(1, None),
            ),
            patch.object(learner, "_is_openai_desktop_target", return_value=False),
            patch.object(learner, "_is_qoder_desktop_target", return_value=True),
        ):
            self.assertEqual(
                learner._visual_placeholder(target, prompt, len(prompt), len(prompt)),
                prompt,
            )

    def test_qoder_helper_uses_committed_value_write(self):
        learner = CorrectionLearner.__new__(CorrectionLearner)
        target = type("Target", (), {"pid": 12345})()

        with patch.object(
            CorrectionLearner,
            "_target_bundle_identifier",
            return_value="com.qoder.ide.helper",
        ):
            self.assertTrue(learner.prefers_value_write(target))

    def test_qoder_and_vscode_use_login_session_text_events(self):
        learner = CorrectionLearner.__new__(CorrectionLearner)
        target = type("Target", (), {"pid": 12345})()

        for bundle_id in ("com.qoder.ide.helper", "com.microsoft.VSCode"):
            with self.subTest(bundle_id=bundle_id), patch.object(
                CorrectionLearner,
                "_target_bundle_identifier",
                return_value=bundle_id,
            ):
                self.assertTrue(learner.uses_event_text_input(target))

    def test_event_text_input_uses_combined_session_source_and_unicode(self):
        learner = CorrectionLearner.__new__(CorrectionLearner)
        target = object()
        posted = []
        unicode_payloads = []
        quartz = SimpleNamespace(
            CGEventSourceCreate=lambda state: ("source", state),
            CGEventCreateKeyboardEvent=lambda _source, keycode, down: {
                "keycode": keycode,
                "down": down,
            },
            CGEventKeyboardSetUnicodeString=lambda event, length, text: unicode_payloads.append(
                (event, length, text)
            ),
            CGEventPost=lambda _tap, event: posted.append(event),
            kCGEventSourceStateCombinedSessionState=1,
            kCGSessionEventTap=2,
        )

        with (
            patch.object(learner, "target_is_focused", return_value=True),
            patch.object(
                learner,
                "_snapshot",
                side_effect=[
                    ("", 0, 0),
                    ("你好", 2, 2),
                ],
            ),
            patch.dict(sys.modules, {"Quartz": quartz}),
            patch("src.correction_learning.time.sleep", return_value=None),
        ):
            self.assertTrue(learner.insert_text_event(target, "你好"))

        self.assertEqual([(item["keycode"], item["down"]) for item in posted], [(49, True), (49, False)])
        self.assertEqual(len(unicode_payloads), 1)
        self.assertEqual(unicode_payloads[0][2], "你好")

    def test_ignored_unicode_carrier_is_removed_without_leaking_a(self):
        learner = CorrectionLearner.__new__(CorrectionLearner)
        target = object()
        posted = []
        quartz = SimpleNamespace(
            CGEventSourceCreate=lambda state: ("source", state),
            CGEventCreateKeyboardEvent=lambda _source, keycode, down: {
                "keycode": keycode,
                "down": down,
            },
            CGEventKeyboardSetUnicodeString=lambda _event, _length, _text: None,
            CGEventPost=lambda _tap, event: posted.append(event),
            kCGEventSourceStateCombinedSessionState=1,
            kCGSessionEventTap=2,
        )

        with (
            patch.object(learner, "target_is_focused", return_value=True),
            patch.object(
                learner,
                "_snapshot",
                side_effect=[("", 0, 0), (" ", 1, 1), ("", 0, 0)],
            ),
            patch.dict(sys.modules, {"Quartz": quartz}),
            patch("src.correction_learning.time.sleep", return_value=None),
        ):
            self.assertFalse(learner.insert_text_event(target, "你好"))

        self.assertEqual(
            [(item["keycode"], item["down"]) for item in posted],
            [(49, True), (49, False), (51, True), (51, False)],
        )
        self.assertNotIn(0, [item["keycode"] for item in posted])

    def test_real_text_is_never_removed_without_matching_placeholder(self):
        self.assertEqual(
            CorrectionLearner._remove_visual_placeholder(
                "Do anything", "", 0, 0
            ),
            ("Do anything", 0, 0),
        )

    def test_missing_target_retains_text_without_touching_clipboard(self):
        """An unknown foreground app must never receive dictated text."""
        manager = self.make_manager()
        manager.capture_output_target = lambda: None
        manager.show_error = lambda _message: None

        pastes = []

        def fake_paste(text, **kwargs):
            pastes.append((text, kwargs))
            return True

        with (
            patch(
                "src.keyboard.listener.paste_text_preserving_clipboard",
                side_effect=fake_paste,
            ),
            patch("src.keyboard.listener.time.sleep", return_value=None),
        ):
            inserted = manager.type_text("盲粘测试", None)

        self.assertFalse(inserted)
        self.assertEqual(pastes, [])
        self.assertEqual(manager.recovery_texts(), ["盲粘测试"])

    def test_blind_paste_failure_falls_to_recovery(self):
        manager = self.make_manager()
        manager.capture_output_target = lambda: None
        manager.show_error = lambda _message: None

        with (
            patch(
                "src.keyboard.listener.paste_text_preserving_clipboard",
                return_value=False,
            ),
            patch("src.keyboard.listener.time.sleep", return_value=None),
        ):
            inserted = manager.type_text("盲粘失败", None)

        self.assertFalse(inserted)
        self.assertEqual(manager.recovery_texts(), ["盲粘失败"])

    def test_blind_paste_normalizes_line_breaks(self):
        manager = self.make_manager()
        manager.capture_output_target = lambda: None
        manager.show_error = lambda _message: None

        pastes = []

        def fake_paste(text, **kwargs):
            pastes.append(text)
            return True

        with (
            patch(
                "src.keyboard.listener.paste_text_preserving_clipboard",
                side_effect=fake_paste,
            ),
            patch("src.keyboard.listener.time.sleep", return_value=None),
        ):
            manager.type_text("第一段\n第二段\r\n结尾", None)

        self.assertEqual(pastes, [])
        self.assertEqual(manager.recovery_texts(), ["第一段 第二段 结尾"])

    def test_explicitly_missing_recording_target_is_not_recaptured_at_commit(self):
        manager = self.make_manager()
        manager.show_error = lambda _message: None
        with patch.object(manager, "capture_output_target") as capture, patch(
            "src.keyboard.listener.paste_text_preserving_clipboard"
        ) as paste:
            self.assertFalse(manager.type_text("safe recovery", target=None))
        capture.assert_not_called()
        paste.assert_not_called()
        self.assertEqual(manager.recovery_texts(), ["safe recovery"])

    def test_blind_paste_exception_retains_for_recovery(self):
        manager = self.make_manager()
        manager.capture_output_target = lambda: None
        manager.show_error = lambda _message: None

        with (
            patch(
                "src.keyboard.listener.paste_text_preserving_clipboard",
                side_effect=RuntimeError("剪贴板炸了"),
            ),
            patch("src.keyboard.listener.time.sleep", return_value=None),
        ):
            inserted = manager.type_text("异常时也要保留", None)

        self.assertFalse(inserted)
        self.assertEqual(manager.recovery_texts(), ["异常时也要保留"])

    def test_focus_change_with_target_still_blocks_blind_path_not_used(self):
        """If we DID capture a target but it lost focus, never blind-paste."""
        manager = self.make_manager(focused=False)
        manager.show_error = lambda _message: None

        blind_called = []

        with (
            patch(
                "src.keyboard.listener.paste_text_preserving_clipboard",
                side_effect=lambda *a, **kw: blind_called.append(1) or True,
            ),
            patch("src.keyboard.listener.time.sleep", return_value=None),
        ):
            inserted = manager.type_text(
                "焦点丢了绝不能盲粘", None, target=manager.correction_learner.target
            )

        self.assertFalse(inserted)
        self.assertEqual(manager.recovery_texts(), ["焦点丢了绝不能盲粘"])
        self.assertEqual(blind_called, [])


if __name__ == "__main__":
    unittest.main()
