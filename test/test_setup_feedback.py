import unittest
from src.setup_feedback import empty_result_notice


class SetupFeedbackTests(unittest.TestCase):
    def test_short_hold_explains_hold_instead_of_blaming_microphone(self):
        outcome, notice = empty_result_notice(130, 130, "hold", "右 Option")
        self.assertEqual(outcome, "too_short")
        self.assertIn("按住", notice)
        self.assertNotIn("检查麦克风", notice)

    def test_short_toggle_explains_toggle(self):
        _, notice = empty_result_notice(130, 130, "toggle", "右 Option")
        self.assertIn("再按", notice)

    def test_silence_and_provider_empty_are_distinct(self):
        self.assertEqual(empty_result_notice(2000, 0, "hold", "右 Option")[0], "silence")
        self.assertEqual(empty_result_notice(2000, 1800, "hold", "右 Option")[0], "empty_result")
