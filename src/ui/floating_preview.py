"""A compact, animated waveform shown beside the active text cursor."""

from __future__ import annotations

import math
import os
import traceback
from typing import Optional, Tuple

import objc
from AppKit import (
    NSAnimationContext,
    NSEvent,
    NSBezierPath,
    NSColor,
    NSFont,
    NSLineBreakByTruncatingHead,
    NSMakeRect,
    NSPanel,
    NSPopUpMenuWindowLevel,
    NSRunLoop,
    NSScreen,
    NSTextAlignmentCenter,
    NSTextField,
    NSTimer,
    NSView,
    NSWindowCollectionBehaviorCanJoinAllSpaces,
    NSWindowCollectionBehaviorFullScreenAuxiliary,
    NSWindowStyleMaskBorderless,
    NSWindowStyleMaskNonactivatingPanel,
    NSWorkspace,
)
from ApplicationServices import (
    AXUIElementCopyAttributeValue,
    AXUIElementCopyParameterizedAttributeValue,
    AXUIElementCreateApplication,
    AXValueGetValue,
    kAXBoundsForRangeParameterizedAttribute,
    kAXFocusedUIElementAttribute,
    kAXPositionAttribute,
    kAXSelectedTextRangeAttribute,
    kAXSizeAttribute,
    kAXValueTypeCGPoint,
    kAXValueTypeCGRect,
    kAXValueTypeCGSize,
)
from Foundation import NSRunLoopCommonModes
from PyObjCTools import AppHelper

from src.utils.logger import logger


# Whether the floating wave should try to follow the caret via AX.  Disabled
# by default because AX exposure varies wildly across apps (WeChat 4.x hides
# it entirely, Electron editors return bogus bounds on Tahoe), making the
# position jump around.  A fixed bottom-center spot is predictable everywhere.
# Set VOICE_INPUT_FLOATING_FOLLOW_CARET=1 to restore caret-following.
_FOLLOW_CARET = os.getenv("VOICE_INPUT_FLOATING_FOLLOW_CARET", "0") == "1"

# Distance above the bottom of the visible main-screen frame, in points.
# 0 means flush against the Dock edge (visibleFrame already excludes Dock).
_FIXED_BOTTOM_OFFSET = 0.0


def _ax_rect_in_any_screen(
    ax_x: float, ax_y: float, width: float, height: float
) -> bool:
    """Whether an AX-coordinate rect falls within some physical screen.

    AX reports global coordinates with the origin at the primary screen's
    top-left and y growing downward; NSScreen frames use the bottom-left
    convention.  Some editors (Electron-based VS Code, WeChat 4.x on macOS
    Tahoe) hand back bogus ``AXBoundsForRange`` rects — usually (0, 0) or a
    stale window-relative point — so we sanity-check before trusting one.
    The 50pt tolerance lets the caret sit on the screen edge.
    """
    screens = list(NSScreen.screens())
    if not screens:
        return False
    primary = screens[0].frame()
    ns_y_top = primary.origin.y + primary.size.height - ax_y
    ns_y_bottom = ns_y_top - height
    tolerance = 50.0
    for screen in screens:
        sf = screen.frame()
        if (
            sf.origin.x - tolerance <= ax_x <= sf.origin.x + sf.size.width + tolerance
            and sf.origin.y - tolerance <= ns_y_bottom
            <= sf.origin.y + sf.size.height + tolerance
        ):
            return True
    return False


def _flip_ax_rect_to_ns(
    ax_x: float, ax_y: float, width: float, height: float
) -> Tuple[float, float, float, float]:
    """Convert an AX rect to NSScreen coordinates.

    Returns ``(x, ns_y_bottom, width, height)`` where ``ns_y_bottom`` is the
    rect's bottom edge in the global NS coordinate space — the value you would
    pass to ``NSWindow.setFrame_display_``.
    """
    screens = list(NSScreen.screens())
    primary = screens[0].frame() if screens else NSScreen.mainScreen().frame()
    return (
        ax_x,
        primary.origin.y + primary.size.height - ax_y - height,
        width,
        height,
    )


def _get_caret_position() -> Tuple[float, float, float, float]:
    """Return the caret bounds in NS coordinates, with a layered fallback.

    Three layers because editors behave very differently:
      1. ``AXBoundsForRange`` on the focused element — real caret rectangle.
         Used by Cocoa text views (Terminal, Notes, Mail).  Rejected when the
         rect falls outside every screen, which is the signature of the
         Tahoe / Electron ``AXBoundsForRange`` regression.
      2. Focused element ``position``+``size`` — the whole input control's
         frame.  Coarser than the caret but still anchored to the field the
         user is typing into, instead of the mouse pointer.
      3. Caller drops to ``NSEvent.mouseLocation`` — last resort.
    """
    front_app = NSWorkspace.sharedWorkspace().frontmostApplication()
    if front_app is None:
        raise RuntimeError("无法获取当前激活的应用")

    app_element = AXUIElementCreateApplication(front_app.processIdentifier())
    from ApplicationServices import AXUIElementSetMessagingTimeout
    AXUIElementSetMessagingTimeout(app_element, 0.2)
    error, focused = AXUIElementCopyAttributeValue(
        app_element, kAXFocusedUIElementAttribute, None
    )
    if error != 0 or focused is None:
        raise RuntimeError(f"无法获取焦点元素, error={error}")

    AXUIElementSetMessagingTimeout(focused, 0.2)

    error, selected_range = AXUIElementCopyAttributeValue(
        focused, kAXSelectedTextRangeAttribute, None
    )
    if error == 0 and selected_range is not None:
        error, bounds_value = AXUIElementCopyParameterizedAttributeValue(
            focused,
            kAXBoundsForRangeParameterizedAttribute,
            selected_range,
            None,
        )
        if error == 0 and bounds_value is not None:
            success, rect = AXValueGetValue(bounds_value, kAXValueTypeCGRect, None)
            # height > 0 rules out the degenerate zero-height rect some editors
            # return; the on-screen check rules out the bogus far-from-screen
            # rects Tahoe / Electron can hand back.  Width is left to the
            # on-screen check, so a multi-thousand-point selection still works.
            if success and rect.size.height > 0:
                if _ax_rect_in_any_screen(
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                ):
                    return _flip_ax_rect_to_ns(
                        rect.origin.x,
                        rect.origin.y,
                        max(rect.size.width, 2.0),
                        rect.size.height,
                    )
                logger.debug(
                    "AXBoundsForRange rect 不在任何屏幕内, 走 fallback: "
                    "origin=(%s, %s) size=(%s, %s)",
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                )

    error, position_value = AXUIElementCopyAttributeValue(
        focused, kAXPositionAttribute, None
    )
    if error != 0 or position_value is None:
        raise RuntimeError(f"无法获取位置属性, error={error}")
    error, size_value = AXUIElementCopyAttributeValue(focused, kAXSizeAttribute, None)
    if error != 0 or size_value is None:
        raise RuntimeError(f"无法获取尺寸属性, error={error}")

    position_ok, point = AXValueGetValue(position_value, kAXValueTypeCGPoint, None)
    size_ok, size = AXValueGetValue(size_value, kAXValueTypeCGSize, None)
    if not position_ok or not size_ok:
        raise RuntimeError("无法解析焦点元素位置")
    if not _ax_rect_in_any_screen(point.x, point.y, size.width, size.height):
        raise RuntimeError("焦点元素位置不在屏幕内")
    return _flip_ax_rect_to_ns(point.x, point.y, size.width, size.height)


def _frame_contains(frame, x: float, y: float) -> bool:
    return (
        frame.origin.x <= x <= frame.origin.x + frame.size.width
        and frame.origin.y <= y <= frame.origin.y + frame.size.height
    )


def _screen_for_point(x: float, y: float):
    screens = list(NSScreen.screens())
    for screen in screens:
        if _frame_contains(screen.frame(), x, y):
            return screen
    if not screens:
        return NSScreen.mainScreen()
    return min(
        screens,
        key=lambda screen: (
            x - (screen.frame().origin.x + screen.frame().size.width / 2)
        ) ** 2
        + (
            y - (screen.frame().origin.y + screen.frame().size.height / 2)
        ) ** 2,
    )


def _clamp_to_frame(
    x: float,
    y: float,
    width: float,
    height: float,
    frame,
    margin: float = 10.0,
) -> tuple[float, float]:
    minimum_x = frame.origin.x + margin
    maximum_x = frame.origin.x + frame.size.width - width - margin
    minimum_y = frame.origin.y + margin
    maximum_y = frame.origin.y + frame.size.height - height - margin
    return (
        max(minimum_x, min(x, maximum_x)),
        max(minimum_y, min(y, maximum_y)),
    )


class _WaveformView(NSView):
    """Draw an audio-reactive cyan/blue/violet bar wave."""

    def initWithFrame_(self, frame):
        self = objc.super(_WaveformView, self).initWithFrame_(frame)
        if self is None:
            return None
        self._phase = 0.0
        self._level = 0.04
        self._target_level = 0.04
        self.setWantsLayer_(True)
        return self

    def isOpaque(self):
        return False

    def setAudioLevel_(self, level):
        self._target_level = max(0.025, min(float(level), 1.0))

    def animate_(self, _timer):
        self._phase += 0.18
        # Quick attack and gentle decay feels responsive without jittering.
        rate = 0.58 if self._target_level > self._level else 0.16
        self._level += (self._target_level - self._level) * rate
        self._target_level = max(0.025, self._target_level * 0.82)
        self.setNeedsDisplay_(True)

    def drawRect_(self, _dirty_rect):
        bounds = self.bounds()
        background = NSBezierPath.bezierPathWithRoundedRect_xRadius_yRadius_(
            bounds, bounds.size.height / 2, bounds.size.height / 2
        )
        NSColor.colorWithCalibratedWhite_alpha_(0.08, 0.88).setFill()
        background.fill()

        bar_count = 17
        bar_width = 3.2
        gap = 4.0
        total_width = bar_count * bar_width + (bar_count - 1) * gap
        start_x = (bounds.size.width - total_width) / 2
        center_y = 14.0

        for index in range(bar_count):
            distance = abs(index - (bar_count - 1) / 2) / (bar_count / 2)
            envelope = max(0.30, 1.0 - distance * 0.58)
            motion = 0.62 + 0.38 * math.sin(self._phase + index * 0.72) ** 2
            secondary = 0.82 + 0.18 * math.sin(self._phase * 0.61 - index * 0.31)
            height = 3.5 + 19.0 * self._level * envelope * motion * secondary
            height = max(3.5, min(height, 23.0))
            x = start_x + index * (bar_width + gap)
            rect = NSMakeRect(x, center_y - height / 2, bar_width, height)
            bar = NSBezierPath.bezierPathWithRoundedRect_xRadius_yRadius_(
                rect, bar_width / 2, bar_width / 2
            )

            # Slowly breathe through cyan → blue → violet. Keeping the hue
            # inside this band avoids the traffic-light red/yellow look.
            color_wave = (math.sin(self._phase * 0.055 + index * 0.24) + 1.0) / 2.0
            hue = 0.51 + 0.24 * color_wave
            NSColor.colorWithCalibratedHue_saturation_brightness_alpha_(
                hue, 0.72, 1.0, 0.98
            ).setFill()
            bar.fill()


class FloatingPreviewWindow:
    """Borderless waveform panel that never steals focus."""

    def __init__(self) -> None:
        self._panel: Optional[NSPanel] = None
        self._waveform: Optional[_WaveformView] = None
        self._text_label: Optional[NSTextField] = None
        self._timer: Optional[NSTimer] = None
        self._is_visible = False
        self._animation_generation = 0
        self._width = 320.0
        self._height = 58.0

    def show(self) -> None:
        AppHelper.callAfter(self._show_now)

    def _show_now(self) -> None:
        """Main-thread rendering shared by dictation and brief static notices."""
        if self._panel is None:
            self._create_panel()
        self._animation_generation += 1
        if _FOLLOW_CARET:
            self._position_near_caret()
        else:
            self._position_fixed()
        if self._text_label is not None:
            self._text_label.setStringValue_("")
        self._start_animation()
        self._panel.setAlphaValue_(0.0)
        self._panel.orderFrontRegardless()
        self._is_visible = True

        def animations(context) -> None:
            context.setDuration_(0.24)
            self._panel.animator().setAlphaValue_(1.0)

        NSAnimationContext.runAnimationGroup_completionHandler_(animations, None)

    def show_notice(self, message: str, *, is_relevant) -> None:
        """Nonactivating, static, and short-lived; never dismiss a newer recording."""
        def display() -> None:
            if not is_relevant():
                return
            self._show_now()
            self._stop_animation()
            self._text_label.setStringValue_(message)
            generation = self._animation_generation

            def dismiss() -> None:
                if generation == self._animation_generation:
                    self.hide(expected_generation=generation)

            AppHelper.callLater(4, dismiss)

        AppHelper.callAfter(display)

    def hide(self, *, expected_generation: int | None = None) -> None:
        def _hide() -> None:
            if expected_generation is not None and expected_generation != self._animation_generation:
                return
            if self._panel is None:
                return
            self._animation_generation += 1
            generation = self._animation_generation
            self._is_visible = False

            def animations(context) -> None:
                context.setDuration_(0.34)
                self._panel.animator().setAlphaValue_(0.0)

            def completed() -> None:
                if generation != self._animation_generation or self._is_visible:
                    return
                self._stop_animation()
                self._panel.orderOut_(None)

            NSAnimationContext.runAnimationGroup_completionHandler_(animations, completed)

        AppHelper.callAfter(_hide)

    def update_level(self, level: float) -> None:
        def _update() -> None:
            if self._waveform is not None:
                self._waveform.setAudioLevel_(level)

        AppHelper.callAfter(_update)

    def update_text(self, text: str) -> None:
        """Show the latest hypothesis in the capsule without editing the target."""
        text = " ".join((text or "").split())

        def _update() -> None:
            if self._text_label is not None:
                self._text_label.setStringValue_(text)

        AppHelper.callAfter(_update)

    def _start_animation(self) -> None:
        if self._timer is not None or self._waveform is None:
            return
        self._timer = NSTimer.timerWithTimeInterval_target_selector_userInfo_repeats_(
            1.0 / 30.0,
            self._waveform,
            "animate:",
            None,
            True,
        )
        NSRunLoop.mainRunLoop().addTimer_forMode_(self._timer, NSRunLoopCommonModes)

    def _stop_animation(self) -> None:
        if self._timer is not None:
            self._timer.invalidate()
            self._timer = None

    def _position_fixed(self) -> None:
        """Anchor the panel at a stable bottom-center spot on the main screen.

        Caret-following varies across apps (no AX, bogus AX bounds), so the
        default is a predictable position.  ``_FIXED_BOTTOM_OFFSET`` sits
        above the Dock without crowding the working area.
        """
        if self._panel is None:
            return
        screen = NSScreen.mainScreen().visibleFrame()
        x = screen.origin.x + (screen.size.width - self._width) / 2
        y = screen.origin.y + _FIXED_BOTTOM_OFFSET
        self._panel.setFrame_display_(
            NSMakeRect(x, y, self._width, self._height), True
        )

    def _position_near_caret(self) -> None:
        if self._panel is None:
            return
        try:
            caret_x, caret_y, _caret_width, caret_height = _get_caret_position()
            screen = _screen_for_point(caret_x, caret_y)
            visible_frame = screen.visibleFrame()
            if caret_height > visible_frame.size.height:
                raise RuntimeError("辅助功能返回的光标位置不合理")
            x = caret_x
            y = caret_y - self._height - 8
            if y < visible_frame.origin.y + 8:
                y = caret_y + caret_height + 8
        except Exception as exc:  # noqa: BLE001
            logger.debug("声波无法跟随光标，改用鼠标附近位置: %s", exc)
            logger.debug(traceback.format_exc())
            mouse = NSEvent.mouseLocation()
            screen = _screen_for_point(mouse.x, mouse.y)
            visible_frame = screen.visibleFrame()
            x = mouse.x - self._width / 2
            y = mouse.y - self._height - 18
        x, y = _clamp_to_frame(
            x,
            y,
            self._width,
            self._height,
            visible_frame,
        )
        self._panel.setFrame_display_(
            NSMakeRect(x, y, self._width, self._height), True
        )

    def _create_panel(self) -> None:
        screen = NSScreen.mainScreen().visibleFrame()
        frame = NSMakeRect(
            screen.origin.x + (screen.size.width - self._width) / 2,
            screen.origin.y + screen.size.height - 140,
            self._width,
            self._height,
        )
        style = NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
        self._panel = NSPanel.alloc().initWithContentRect_styleMask_backing_defer_(
            frame, style, 2, False
        )
        self._panel.setLevel_(NSPopUpMenuWindowLevel)
        self._panel.setCollectionBehavior_(
            NSWindowCollectionBehaviorCanJoinAllSpaces
            | NSWindowCollectionBehaviorFullScreenAuxiliary
        )
        self._panel.setHidesOnDeactivate_(False)
        self._panel.setCanHide_(False)
        self._panel.setOpaque_(False)
        self._panel.setBackgroundColor_(NSColor.clearColor())
        self._panel.setHasShadow_(True)
        self._panel.setIgnoresMouseEvents_(True)
        self._panel.setAlphaValue_(0.0)

        self._waveform = _WaveformView.alloc().initWithFrame_(
            NSMakeRect(0, 0, self._width, self._height)
        )
        self._text_label = NSTextField.labelWithString_("")
        self._text_label.setFrame_(NSMakeRect(16, 32, self._width - 32, 18))
        self._text_label.setAlignment_(NSTextAlignmentCenter)
        self._text_label.setFont_(NSFont.systemFontOfSize_weight_(13.0, 0.25))
        self._text_label.setTextColor_(
            NSColor.whiteColor().colorWithAlphaComponent_(0.92)
        )
        self._text_label.setUsesSingleLineMode_(True)
        self._text_label.cell().setLineBreakMode_(NSLineBreakByTruncatingHead)
        self._waveform.addSubview_(self._text_label)
        self._panel.setContentView_(self._waveform)
