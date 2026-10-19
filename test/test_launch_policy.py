import unittest
from unittest.mock import patch

from main import VoiceAssistant, _should_open_settings_on_start


class LaunchPolicyTests(unittest.TestCase):
    def test_saved_key_does_not_hide_unfinished_setup_on_manual_launch(self):
        self.assertTrue(_should_open_settings_on_start(
            processor_present=True, setup_complete=False, is_frozen=True,
            arguments=["VeyqaVoice"], restarting=False,
        ))
        self.assertFalse(_should_open_settings_on_start(
            processor_present=True, setup_complete=False, is_frozen=True,
            arguments=["VeyqaVoice", "--background-login"], restarting=False,
        ))

    def test_background_login_never_opens_settings(self):
        for processor_present in (False, True):
            self.assertFalse(
                _should_open_settings_on_start(
                    processor_present=processor_present,
                    is_frozen=True,
                    arguments=["VeyqaVoice", "--background-login"],
                    restarting=False,
                )
            )

    def test_only_unconfigured_launch_automatically_opens_settings(self):
        self.assertTrue(
            _should_open_settings_on_start(
                processor_present=False,
                is_frozen=True,
                arguments=["VeyqaVoice"],
                restarting=False,
            )
        )
        self.assertFalse(
            _should_open_settings_on_start(
                processor_present=True,
                is_frozen=True,
                arguments=["VeyqaVoice"],
                restarting=False,
            )
        )

    def test_background_restart_does_not_reopen_settings(self):
        self.assertFalse(
            _should_open_settings_on_start(
                processor_present=False,
                is_frozen=True,
                arguments=["VeyqaVoice"],
                restarting=True,
            )
        )

    def test_explicit_restart_reopens_settings(self):
        self.assertTrue(
            _should_open_settings_on_start(
                processor_present=True,
                is_frozen=True,
                arguments=["VeyqaVoice", "--show-settings-after-restart"],
                restarting=True,
            )
        )

    def test_restart_exec_requests_settings_reopen_without_duplicate_flag(self):
        with (
            patch("main.sys.argv", ["VeyqaVoice", "--show-settings-after-restart"]),
            patch("main.os.execv") as execv,
        ):
            VoiceAssistant._restart_application()

        self.assertEqual(
            execv.call_args.args[1],
            [execv.call_args.args[0], "--show-settings-after-restart"],
        )


if __name__ == "__main__":
    unittest.main()
