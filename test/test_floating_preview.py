"""Unit tests for the floating-preview caret positioning.

These tests pin down the layered caret-bounds fallback introduced to keep the
waveform anchored to the caret when ``AXBoundsForRange`` is unavailable or
returns bogus coordinates.  The regression is visible on Electron-based
editors (VS Code, WeChat 4.x) running on macOS Tahoe, where ``AXBoundsForRange``
hands back off-screen or zero rects.
"""

from __future__ import annotations

import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch

from src.ui import floating_preview as fp


class NoticeTests(unittest.TestCase):
    def test_notice_stops_animation_and_old_timer_cannot_hide_next_recording(self):
        preview = fp.FloatingPreviewWindow()
        preview._text_label = Mock()
        preview._show_now = Mock(side_effect=lambda: setattr(preview, "_animation_generation", 10))
        preview._stop_animation = Mock()
        preview.hide = Mock()
        with patch.object(fp.AppHelper, "callAfter", side_effect=lambda fn: fn()), \
             patch.object(fp.AppHelper, "callLater") as later:
            preview.show_notice("文字已保留", is_relevant=lambda: True)
        preview._text_label.setStringValue_.assert_called_with("文字已保留")
        preview._stop_animation.assert_called_once()
        dismiss = later.call_args.args[1]
        preview._animation_generation += 1
        dismiss()
        preview.hide.assert_not_called()

    def test_late_notice_does_not_replace_new_recording(self):
        preview = fp.FloatingPreviewWindow()
        preview._show_now = Mock()
        with patch.object(fp.AppHelper, "callAfter", side_effect=lambda fn: fn()):
            preview.show_notice("旧会话失败", is_relevant=lambda: False)
        preview._show_now.assert_not_called()

    def test_queued_hide_checks_generation_when_it_actually_runs(self):
        preview = fp.FloatingPreviewWindow()
        preview._panel = Mock()
        preview._animation_generation = 10
        with patch.object(fp.AppHelper, "callAfter") as queued:
            preview.hide(expected_generation=10)
        preview._animation_generation = 11
        queued.call_args.args[0]()
        preview._panel.animator.assert_not_called()


class AnimationLifecycleTests(unittest.TestCase):
    def test_stale_preparation_cannot_reopen_cancelled_session(self):
        preview = fp.FloatingPreviewWindow()
        preview._show_now = Mock()
        with patch.object(fp.AppHelper, "callAfter") as queued:
            preview.show(preparing=True, is_relevant=lambda: False)
        queued.call_args.args[0]()
        preview._show_now.assert_not_called()

    def test_preparation_does_not_animate_as_if_already_recording(self):
        preview = fp.FloatingPreviewWindow()
        preview._show_now = Mock()
        preview._text_label = Mock()
        preview._stop_animation = Mock()
        with patch.object(fp.AppHelper, "callAfter", side_effect=lambda fn: fn()):
            preview.show(preparing=True, is_relevant=lambda: True)
        preview._stop_animation.assert_called_once()
        preview._text_label.setStringValue_.assert_called_with("准备录音…")

    def test_native_waveform_reuses_palette_and_reduced_motion_tracks_level(self):
        wave = fp._WaveformView.alloc().initWithFrame_(fp.NSMakeRect(0, 0, 320, 58))
        colors = wave._colors
        self.assertEqual(len(colors), 17)
        wave.animate_(None)
        self.assertIs(wave._colors, colors)
        wave.resetMotion_(True)
        wave.setAudioLevel_(0.6)
        self.assertEqual(wave._phase, 0.0)
        self.assertEqual(wave._level, 0.6)

    def test_reduced_motion_never_starts_a_frame_timer(self):
        preview = fp.FloatingPreviewWindow()
        preview._waveform = Mock()
        preview._reduced_motion = True
        with patch.object(fp, "NSTimer") as timer:
            preview._start_animation()
        timer.timerWithTimeInterval_target_selector_userInfo_repeats_.assert_not_called()

    def test_processing_stops_wave_timer_without_delaying_recognition(self):
        preview = fp.FloatingPreviewWindow()
        timer = preview._timer = Mock()
        preview._waveform = Mock()
        with patch.object(fp.AppHelper, "callAfter", side_effect=lambda fn: fn()):
            preview.set_processing()
        timer.invalidate.assert_called_once()
        self.assertIsNone(preview._timer)
        preview._waveform.resetMotion_.assert_called_with(True)

    def test_duplicate_preview_text_does_not_redraw(self):
        preview = fp.FloatingPreviewWindow()
        preview._text_label = Mock()
        with patch.object(fp.AppHelper, "callAfter", side_effect=lambda fn: fn()):
            preview.update_text("同一句话")
            preview.update_text("同一句话")
        preview._text_label.setStringValue_.assert_called_once_with("同一句话")

    def test_hide_invalidates_timer_before_fade_finishes(self):
        preview = fp.FloatingPreviewWindow()
        preview._panel = Mock()
        timer = preview._timer = Mock()
        with patch.object(fp.AppHelper, "callAfter", side_effect=lambda fn: fn()), \
             patch.object(fp, "NSAnimationContext"):
            preview.hide()
        timer.invalidate.assert_called_once()
        self.assertIsNone(preview._timer)


class _Point(SimpleNamespace):
    def __init__(self, x: float, y: float) -> None:
        super().__init__(x=x, y=y)


class _Size(SimpleNamespace):
    def __init__(self, width: float, height: float) -> None:
        super().__init__(width=width, height=height)


class _Rect(SimpleNamespace):
    def __init__(self, x: float, y: float, w: float, h: float) -> None:
        super().__init__(origin=_Point(x, y), size=_Size(w, h))


class _Frame:
    """NSScreen.frame() return shape."""

    def __init__(self, x: float, y: float, w: float, h: float) -> None:
        self.origin = _Point(x, y)
        self.size = _Size(w, h)


def _make_screens(*specs):
    screens = []
    for (x, y, w, h) in specs:
        frame = _Frame(x, y, w, h)
        screen = SimpleNamespace()
        screen.frame = lambda *a, b=frame: b
        screen.visibleFrame = lambda *a, b=frame: b
        screens.append(screen)
    return screens


def _make_nsscreen(*specs):
    """Build a fake ``NSScreen`` class-like object.

    Replacing the whole ``NSScreen`` reference avoids touching PyObjC's
    Objective-C selectors, which cannot be ``delattr``-ed by ``mock.patch``.
    """
    screens = _make_screens(*specs)
    main_screen = screens[0] if screens else SimpleNamespace(
        frame=lambda *a: _Frame(0, 0, 1440, 900)
    )
    return SimpleNamespace(
        screens=lambda: screens,
        mainScreen=lambda: main_screen,
    )


class AxRectInAnyScreenTests(unittest.TestCase):
    def setUp(self):
        self._patch = patch(
            "src.ui.floating_preview.NSScreen",
            _make_nsscreen((0, 0, 1440, 900)),
        )
        self._patch.start()

    def tearDown(self):
        self._patch.stop()

    def test_rect_inside_screen_is_accepted(self):
        self.assertTrue(fp._ax_rect_in_any_screen(100, 100, 20, 30))

    def test_rect_at_top_left_origin_is_accepted(self):
        self.assertTrue(fp._ax_rect_in_any_screen(0, 0, 20, 30))

    def test_rect_far_below_screen_is_rejected(self):
        # AX y > 900 means below the primary screen; tolerance is 50pt
        self.assertFalse(fp._ax_rect_in_any_screen(100, 1100, 20, 30))

    def test_rect_far_to_the_left_is_rejected(self):
        self.assertFalse(fp._ax_rect_in_any_screen(-1000, 100, 20, 30))

    def test_rect_within_tolerance_is_accepted(self):
        # AX y=920 + height 30 → bottom edge at AX y=950 → 50pt past screen
        self.assertTrue(fp._ax_rect_in_any_screen(100, 920, 20, 30))

    def test_no_screens_returns_false(self):
        with patch("src.ui.floating_preview.NSScreen", _make_nsscreen()):
            self.assertFalse(fp._ax_rect_in_any_screen(100, 100, 20, 30))


class AxRectMultiScreenTests(unittest.TestCase):
    def setUp(self):
        # Primary 1440x900 at (0,0); secondary to the right at (1440, 0)
        self._patch = patch(
            "src.ui.floating_preview.NSScreen",
            _make_nsscreen(
                (0, 0, 1440, 900),
                (1440, 0, 1920, 1080),
            ),
        )
        self._patch.start()

    def tearDown(self):
        self._patch.stop()

    def test_rect_on_primary_screen_is_accepted(self):
        self.assertTrue(fp._ax_rect_in_any_screen(100, 100, 20, 30))

    def test_rect_on_secondary_screen_is_accepted(self):
        # Secondary origin.x = 1440, origin.y = 0, size 1920x1080.
        # In NS coords its Y range is [0, 1080]; in AX coords (y flipped) the
        # point (1500, 400) maps to NS y = 900 - 400 = 500 → on the secondary.
        self.assertTrue(fp._ax_rect_in_any_screen(1500, 400, 20, 30))

    def test_rect_far_below_all_screens_is_rejected(self):
        self.assertFalse(fp._ax_rect_in_any_screen(100, 5000, 20, 30))


class FlipAxToNsTests(unittest.TestCase):
    def test_single_screen_flips_y(self):
        with patch("src.ui.floating_preview.NSScreen", _make_nsscreen((0, 0, 1440, 900))):
            x, y, w, h = fp._flip_ax_rect_to_ns(100, 200, 30, 40)
        self.assertEqual((x, y, w, h), (100, 900 - 200 - 40, 30, 40))

    def test_multi_screen_uses_primary_height_for_flip(self):
        # AX global origin is the primary screen's top-left regardless of how
        # many screens exist.  Flipping always uses the primary's NS height.
        with patch(
            "src.ui.floating_preview.NSScreen",
            _make_nsscreen(
                (0, 0, 1440, 900),
                (1440, 0, 1920, 1080),
            ),
        ):
            x, y, w, h = fp._flip_ax_rect_to_ns(1500, 100, 30, 40)
        self.assertEqual((x, y, w, h), (1500, 900 - 100 - 40, 30, 40))

    def test_empty_screens_falls_back_to_main_screen(self):
        main_frame = _Frame(0, 0, 1440, 900)
        fake = SimpleNamespace(
            screens=lambda: [],
            mainScreen=lambda: SimpleNamespace(frame=lambda *a: main_frame),
        )
        with patch("src.ui.floating_preview.NSScreen", fake):
            x, y, w, h = fp._flip_ax_rect_to_ns(100, 200, 30, 40)
        self.assertEqual((x, y, w, h), (100, 900 - 200 - 40, 30, 40))


class GetCaretPositionTests(unittest.TestCase):
    """Verify the three-layer caret-bounds fallback."""

    def setUp(self):
        self._screen_patch = patch(
            "src.ui.floating_preview.NSScreen",
            _make_nsscreen((0, 0, 1440, 900)),
        )
        self._screen_patch.start()
        self._front = SimpleNamespace(processIdentifier=lambda: 1234)
        workspace = SimpleNamespace(
            sharedWorkspace=lambda: SimpleNamespace(
                frontmostApplication=lambda: self._front
            )
        )
        self._workspace_patch = patch(
            "src.ui.floating_preview.NSWorkspace", workspace
        )
        self._workspace_patch.start()

    def tearDown(self):
        self._screen_patch.stop()
        self._workspace_patch.stop()

    def _run(
        self,
        *,
        selected_range=None,
        bounds_rect=None,
        position=None,
        size=None,
        fail_focus: bool = False,
        fail_position: bool = False,
        fail_size: bool = False,
    ):
        def copy(_element, attr, _):
            if attr == fp.kAXFocusedUIElementAttribute:
                return (1, None) if fail_focus else (0, "focused")
            if attr == fp.kAXSelectedTextRangeAttribute:
                return (
                    (0, selected_range)
                    if selected_range is not None
                    else (1, None)
                )
            if attr == fp.kAXPositionAttribute:
                return (1, None) if fail_position or position is None else (0, "pos")
            if attr == fp.kAXSizeAttribute:
                return (1, None) if fail_size or size is None else (0, "size")
            return (1, None)

        def copy_param(_element, _attr, _range, _):
            if bounds_rect is None:
                return (1, None)
            return (0, "bounds")

        def get_value(value, _kind, _):
            if value == "bounds":
                return (True, bounds_rect)
            if value == "pos":
                return (True, position)
            if value == "size":
                return (True, size)
            return (False, None)

        with (
            patch(
                "src.ui.floating_preview.AXUIElementCreateApplication",
                lambda pid: "app",
            ),
            patch(
                "src.ui.floating_preview.AXUIElementCopyAttributeValue",
                side_effect=copy,
            ),
            patch(
                "src.ui.floating_preview.AXUIElementCopyParameterizedAttributeValue",
                side_effect=copy_param,
            ),
            patch(
                "src.ui.floating_preview.AXValueGetValue",
                side_effect=get_value,
            ),
        ):
            return fp._get_caret_position()

    def test_l1_uses_caret_bounds_when_on_screen(self):
        # AX rect at (100, 200), size 20x30 → NS y = 900 - 200 - 30 = 670
        result = self._run(
            selected_range=(0, 0),
            bounds_rect=_Rect(100, 200, 20, 30),
        )
        self.assertEqual(result, (100, 670, 20, 30))

    def test_l1_width_normalised_to_at_least_two(self):
        # Zero-width caret bounds should be widened for display
        result = self._run(
            selected_range=(0, 0),
            bounds_rect=_Rect(100, 200, 0, 30),
        )
        self.assertEqual(result, (100, 670, 2.0, 30))

    def test_l1_offscreen_falls_back_to_l2(self):
        # Tahoe/Electron regression: AX returns bogus off-screen rect
        result = self._run(
            selected_range=(0, 0),
            bounds_rect=_Rect(5000, 5000, 20, 30),
            position=_Point(300, 400),
            size=_Size(500, 25),
        )
        # L2: AX (300, 400) size 500x25 → NS y = 900 - 400 - 25 = 475
        self.assertEqual(result, (300, 475, 500, 25))

    def test_l1_wide_selection_still_accepted(self):
        # A wide caret rect (long selection) that still fits on the screen
        # must NOT be rejected just because of its size.
        result = self._run(
            selected_range=(0, 0),
            bounds_rect=_Rect(100, 100, 1200, 30),
        )
        self.assertEqual(result, (100, 900 - 100 - 30, 1200, 30))

    def test_no_selected_range_uses_l2_directly(self):
        result = self._run(
            selected_range=None,
            position=_Point(300, 400),
            size=_Size(500, 25),
        )
        self.assertEqual(result, (300, 475, 500, 25))

    def test_l1_and_l2_offscreen_raises(self):
        with self.assertRaises(RuntimeError):
            self._run(
                selected_range=(0, 0),
                bounds_rect=_Rect(5000, 5000, 20, 30),
                position=_Point(-9999, -9999),
                size=_Size(10, 10),
            )

    def test_l1_zero_height_rejected_falls_through(self):
        # Zero-height rect is meaningless; L2 should kick in
        result = self._run(
            selected_range=(0, 0),
            bounds_rect=_Rect(100, 200, 20, 0),
            position=_Point(300, 400),
            size=_Size(500, 25),
        )
        self.assertEqual(result, (300, 475, 500, 25))

    def test_l1_rect_far_below_screen_rejected(self):
        # AX caret rect extending far past the bottom of the screen is the
        # signature of Tahoe / Electron returning bogus bounds.
        with self.assertRaises(RuntimeError):
            self._run(
                selected_range=(0, 0),
                bounds_rect=_Rect(100, 100, 20, 99999),
                position=_Point(-9999, -9999),  # also fail L2
                size=_Size(10, 10),
            )

    def test_l2_size_far_exceeding_screen_rejected(self):
        with self.assertRaises(RuntimeError):
            self._run(
                selected_range=None,
                position=_Point(100, 100),
                size=_Size(10, 99999),
            )

    def test_no_focused_element_raises(self):
        with self.assertRaises(RuntimeError):
            self._run(fail_focus=True)

    def test_l2_missing_position_raises(self):
        with self.assertRaises(RuntimeError):
            self._run(selected_range=None, fail_position=True)

    def test_l2_missing_size_raises(self):
        with self.assertRaises(RuntimeError):
            self._run(
                selected_range=None,
                position=_Point(100, 100),
                fail_size=True,
            )

    def test_frontmost_app_missing_raises(self):
        with patch(
            "src.ui.floating_preview.NSWorkspace",
            SimpleNamespace(
                sharedWorkspace=lambda: SimpleNamespace(
                    frontmostApplication=lambda: None
                )
            ),
        ):
            with self.assertRaises(RuntimeError):
                fp._get_caret_position()


class PositionFixedTests(unittest.TestCase):
    """The default anchor is bottom-center of the main screen, above the Dock."""

    def _make_window(self):
        """Build a FloatingPreviewWindow with a fake panel that records setFrame."""
        win = fp.FloatingPreviewWindow()
        recorded = {}

        class _Panel:
            def setFrame_display_(self, rect, display):
                recorded["rect"] = rect

        win._panel = _Panel()
        win._width = 320.0
        win._height = 58.0
        return win, recorded

    def test_fixed_position_centers_horizontally_on_main_screen(self):
        win, recorded = self._make_window()
        main_frame = _Frame(0, 0, 1440, 900)
        main_screen = SimpleNamespace(visibleFrame=lambda *a: main_frame)
        with patch("src.ui.floating_preview.NSScreen") as ns_screen:
            ns_screen.mainScreen.return_value = main_screen
            win._position_fixed()
        rect = recorded["rect"]
        # x is centered: (1440 - 320) / 2 = 560
        self.assertEqual(rect.origin.x, 560.0)
        # y is bottom + offset = 0 + 0 = 0 (flush against Dock edge)
        self.assertEqual(rect.origin.y, 0.0)
        self.assertEqual(rect.size.width, 320.0)
        self.assertEqual(rect.size.height, 58.0)

    def test_fixed_position_respects_screen_origin(self):
        # If the main screen is offset (rare, but possible in multi-screen
        # layouts), the fixed position must follow the origin, not assume 0,0.
        win, recorded = self._make_window()
        main_frame = _Frame(100, 50, 1920, 1080)
        main_screen = SimpleNamespace(visibleFrame=lambda *a: main_frame)
        with patch("src.ui.floating_preview.NSScreen") as ns_screen:
            ns_screen.mainScreen.return_value = main_screen
            win._position_fixed()
        rect = recorded["rect"]
        self.assertEqual(rect.origin.x, 100 + (1920 - 320) / 2)
        self.assertEqual(rect.origin.y, 50 + 0)

    def test_fixed_position_no_panel_is_noop(self):
        win = fp.FloatingPreviewWindow()
        win._panel = None
        win._position_fixed()  # must not raise


class ClampToFrameTests(unittest.TestCase):
    def test_inside_frame_unchanged(self):
        frame = _Frame(0, 0, 1000, 800)
        x, y = fp._clamp_to_frame(100, 100, 200, 100, frame)
        self.assertEqual((x, y), (100, 100))

    def test_left_edge_clamped(self):
        frame = _Frame(0, 0, 1000, 800)
        x, y = fp._clamp_to_frame(-50, 100, 200, 100, frame)
        # minimum_x = 0 + 10 = 10
        self.assertEqual(x, 10)
        self.assertEqual(y, 100)

    def test_right_edge_clamped(self):
        frame = _Frame(0, 0, 1000, 800)
        x, y = fp._clamp_to_frame(900, 100, 200, 100, frame)
        # maximum_x = 0 + 1000 - 200 - 10 = 790
        self.assertEqual(x, 790)

    def test_bottom_edge_clamped(self):
        frame = _Frame(0, 0, 1000, 800)
        x, y = fp._clamp_to_frame(100, -50, 200, 100, frame)
        # minimum_y = 0 + 10 = 10
        self.assertEqual(y, 10)


if __name__ == "__main__":
    unittest.main()
