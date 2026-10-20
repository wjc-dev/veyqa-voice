import json
import os
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch, MagicMock

from src.updater import UpdateCommands, settings_block_install


class UpdateCommandTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.commands = UpdateCommands(self.directory)

    def request(self, **changes):
        payload = dict(id="request-1", created_at=1000, action="check")
        payload.update(changes)
        self.commands.path.write_text(json.dumps(payload))
        return self.commands.take(now=1001)

    def test_check_and_duplicate(self):
        self.assertEqual(self.request()["action"], "check")
        self.assertIsNone(self.request())

    def test_only_boolean_automatic_preference(self):
        self.assertIs(self.request(action="automatic", enabled=False)["enabled"], False)
        self.assertIsNone(self.request(id="other", action="automatic", enabled="false"))

    def test_stale_future_unknown_or_remote_payload_rejected(self):
        for changes in [dict(created_at=1), dict(created_at=2000), dict(created_at=True),
                        dict(action="install"), dict(url="https://example.com/app.zip"),
                        dict(id=""), dict(created_at=float("nan"))]:
            with self.subTest(changes=changes):
                self.assertIsNone(self.request(**changes))

    def test_malformed_and_oversized_are_nonfatal(self):
        for data in ["not-json", "[]", "x" * 2048]:
            self.commands.path.write_text(data)
            self.assertIsNone(self.commands.take())

    def test_symlink_is_not_consumed(self):
        target = self.directory / "other"
        target.write_text('{"id":"one"}')
        self.commands.path.symlink_to(target)
        self.assertIsNone(self.commands.take())
        self.assertTrue(target.exists())

    def test_live_draft_blocks_but_saved_or_dead_settings_do_not(self):
        path = self.directory / ".settings-update-state.json"
        self.assertFalse(settings_block_install(self.directory))
        path.write_text(json.dumps(dict(pid=os.getpid(), dirty=True)))
        self.assertTrue(settings_block_install(self.directory))
        path.write_text(json.dumps(dict(pid=os.getpid(), dirty=False)))
        self.assertFalse(settings_block_install(self.directory))
        path.write_text(json.dumps(dict(pid=98765, dirty=True)))
        with patch("src.updater.os.kill", side_effect=ProcessLookupError):
            self.assertFalse(settings_block_install(self.directory))

    def test_invalid_settings_state_fails_closed(self):
        path = self.directory / ".settings-update-state.json"
        path.write_text("{}")
        self.assertTrue(settings_block_install(self.directory))


class UpdateSessionGateTests(unittest.TestCase):
    def setUp(self):
        import main
        self.main = main
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.pause = self.directory / ".supervisor-paused"
        self.assistant = main.VoiceAssistant.__new__(main.VoiceAssistant)
        self.assistant._session_lock = threading.RLock()
        self.assistant._current_session = None
        self.assistant._streaming_thread = None
        self.assistant._microphone_test_active = False
        self.assistant._update_installing = False
        self.assistant._update_pause_previous = None
        self.assistant.keyboard_manager = MagicMock(is_recording=False, _transition_started_at=None)
        self.stack = []
        for context in [patch.object(main, "DATA_DIR", self.directory),
                        patch("src.login_item.SUPERVISOR_PAUSE_FILE", self.pause),
                        patch.dict(os.environ, {"SETUP_COMPLETED": "true"})]:
            context.start()
            self.addCleanup(context.stop)

    def test_idle_can_check_and_prepare_then_cancel_restores_state(self):
        self.assertTrue(self.assistant._update_may_check())
        self.assertTrue(self.assistant._prepare_update())
        self.assertTrue(self.assistant._update_installing)
        self.assertEqual(self.pause.read_text(), "paused-for-update")
        self.assertFalse(self.assistant._update_may_check())
        self.assistant._cancel_update()
        self.assertFalse(self.pause.exists())
        self.assertFalse(self.assistant._update_installing)

    def test_each_busy_state_blocks_without_pausing_supervisor(self):
        for field, value in [("_current_session", object()), ("_microphone_test_active", True),
                             ("_streaming_thread", MagicMock(is_alive=lambda: True))]:
            old = getattr(self.assistant, field)
            setattr(self.assistant, field, value)
            self.assertFalse(self.assistant._prepare_update(), field)
            self.assertFalse(self.assistant._update_may_check(), field)
            setattr(self.assistant, field, old)
        self.assertFalse(self.pause.exists())

    def test_keyboard_transition_and_unsaved_settings_block(self):
        self.assistant.keyboard_manager._transition_started_at = 1
        self.assertFalse(self.assistant._prepare_update())
        self.assistant.keyboard_manager._transition_started_at = None
        (self.directory / ".settings-update-state.json").write_text(json.dumps(dict(pid=os.getpid(), dirty=True)))
        self.assertFalse(self.assistant._prepare_update())

    def test_cancel_preserves_existing_pause_marker(self):
        self.pause.write_text("paused-by-user")
        self.assertTrue(self.assistant._prepare_update())
        self.assistant._cancel_update()
        self.assertEqual(self.pause.read_text(), "paused-by-user")

    def test_no_new_session_can_start_during_install(self):
        self.assistant._update_installing = True
        self.assistant.processor = MagicMock()
        self.assistant._reject_start = MagicMock()
        self.assistant.start_streaming()
        self.assistant._reject_start.assert_called_once()
        self.assertIsNone(self.assistant._current_session)
