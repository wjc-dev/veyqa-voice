from types import SimpleNamespace
from unittest.mock import Mock, patch

import pytest

from src.keyboard.listener import KeyboardManager


@pytest.mark.parametrize("bundle,pid,focused,identifier,expected", [
    ("com.wjcdev.veyqa.settings", 12, True, "veyqa.setup.trial", True),
    ("com.wjcdev.veyqa.settings", 12, True, "api-key", False),
    ("com.wjcdev.veyqa.settings", 12, False, "veyqa.setup.trial", False),
    ("com.wjcdev.veyqa.settings", 99, True, "veyqa.setup.trial", False),
    ("some.other.app", 12, True, "veyqa.setup.trial", False),
])
def test_only_live_settings_trial_editor_is_allowed(bundle, pid, focused, identifier, expected):
    manager = KeyboardManager.__new__(KeyboardManager)
    manager.correction_learner = Mock()
    manager.correction_learner.target_is_focused.return_value = focused
    front = Mock()
    front.bundleIdentifier.return_value = bundle
    front.processIdentifier.return_value = pid
    with patch("AppKit.NSWorkspace") as workspace, \
            patch("ApplicationServices.AXUIElementCopyAttributeValue", return_value=(0, identifier)):
        workspace.sharedWorkspace.return_value.frontmostApplication.return_value = front
        assert manager.is_setup_trial_target(SimpleNamespace(element=object(), pid=12)) is expected


def test_missing_trial_target_is_never_allowed():
    assert KeyboardManager.__new__(KeyboardManager).is_setup_trial_target(None) is False
