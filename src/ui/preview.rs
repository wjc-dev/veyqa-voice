//! Floating 320×58 waveform preview panel.
//!
//! Ported from `src/ui/floating_preview.py`: a borderless nonactivating panel
//! that never steals focus, with a 30 Hz audio-reactive wave and a transient
//! text line for partial hypotheses or notices. Fades are driven by the UI
//! pump (60 ms in, 180 ms out) so no extra timer targets are needed.

use std::cell::Cell;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSColor, NSEvent, NSFont, NSLineBreakMode, NSPanel,
    NSPopUpMenuWindowLevel, NSScreen, NSTextAlignment, NSTextField, NSView, NSWindowStyleMask,
    NSWindowCollectionBehavior, NSWorkspace,
};
use objc2_application_services::{AXError, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{CFRetained, CFString, CFType, CGPoint, CGRect, CGSize};
use objc2_foundation::{ns_string, NSPoint, NSRect, NSSize};

const WIDTH: f64 = 320.0;
const HEIGHT: f64 = 58.0;
const BAR_COUNT: usize = 17;
const NOTICE_SECONDS: f64 = 4.0;
/// Python's `_FIXED_BOTTOM_OFFSET`; `visibleFrame` already excludes the Dock.
const FIXED_BOTTOM_OFFSET: f64 = 0.0;
const CARET_GAP: f64 = 8.0;
const FRAME_MARGIN: f64 = 10.0;
const SCREEN_TOLERANCE: f64 = 50.0;
const AX_CARET_TIMEOUT_SECONDS: f32 = 0.2;

/// Caret-following is opt-in, matching Python's default. AX exposure varies
/// wildly across apps (WeChat 4.x hides it, Electron returns bogus bounds).
fn caret_follow_enabled() -> bool {
    std::env::var("VOICE_INPUT_FLOATING_FOLLOW_CARET")
        .map(|value| value == "1")
        .unwrap_or(false)
}

fn screen_frames(mtm: MainThreadMarker) -> Vec<NSRect> {
    NSScreen::screens(mtm)
        .iter()
        .map(|screen| screen.frame())
        .collect()
}

fn visible_frame_for_point(mtm: MainThreadMarker, frames: &[NSRect], x: f64, y: f64) -> Option<NSRect> {
    let screens = NSScreen::screens(mtm);
    let index = screen_index_for_point(frames, x, y).unwrap_or(0);
    screens
        .iter()
        .nth(index)
        .map(|screen| screen.visibleFrame())
        .or_else(|| NSScreen::mainScreen(mtm).map(|screen| screen.visibleFrame()))
}

fn screen_index_for_point(frames: &[NSRect], x: f64, y: f64) -> Option<usize> {
    if frames.is_empty() {
        return None;
    }
    if let Some(index) = frames.iter().position(|frame| frame_contains(*frame, x, y)) {
        return Some(index);
    }
    let mut best = 0;
    let mut best_distance = f64::INFINITY;
    for (index, frame) in frames.iter().enumerate() {
        let center_x = frame.origin.x + frame.size.width / 2.0;
        let center_y = frame.origin.y + frame.size.height / 2.0;
        let distance = (x - center_x).powi(2) + (y - center_y).powi(2);
        if distance < best_distance {
            best_distance = distance;
            best = index;
        }
    }
    Some(best)
}

fn frame_contains(frame: NSRect, x: f64, y: f64) -> bool {
    frame.origin.x <= x
        && x <= frame.origin.x + frame.size.width
        && frame.origin.y <= y
        && y <= frame.origin.y + frame.size.height
}

/// AX uses a top-left origin on the primary screen; AppKit a bottom-left one.
fn flip_ax_rect(primary: NSRect, ax_x: f64, ax_y: f64, width: f64, height: f64) -> (f64, f64, f64, f64) {
    (
        ax_x,
        primary.origin.y + primary.size.height - ax_y - height,
        width,
        height,
    )
}

fn ax_rect_in_any_screen(
    frames: &[NSRect],
    primary: NSRect,
    ax_x: f64,
    ax_y: f64,
    _width: f64,
    height: f64,
) -> bool {
    if frames.is_empty() {
        return false;
    }
    let ns_y_top = primary.origin.y + primary.size.height - ax_y;
    let ns_y_bottom = ns_y_top - height;
    frames.iter().any(|frame| {
        frame.origin.x - SCREEN_TOLERANCE <= ax_x
            && ax_x <= frame.origin.x + frame.size.width + SCREEN_TOLERANCE
            && frame.origin.y - SCREEN_TOLERANCE <= ns_y_bottom
            && ns_y_bottom <= frame.origin.y + frame.size.height + SCREEN_TOLERANCE
    })
}

fn clamp_to_frame(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    frame: NSRect,
    margin: f64,
) -> (f64, f64) {
    let minimum_x = frame.origin.x + margin;
    let maximum_x = frame.origin.x + frame.size.width - width - margin;
    let minimum_y = frame.origin.y + margin;
    let maximum_y = frame.origin.y + frame.size.height - height - margin;
    (
        minimum_x.max(x.min(maximum_x)),
        minimum_y.max(y.min(maximum_y)),
    )
}

fn copy_ax_attribute(element: &AXUIElement, name: &'static str) -> Option<CFRetained<CFType>> {
    let attribute = CFString::from_static_str(name);
    let mut value: *const CFType = std::ptr::null();
    let error =
        unsafe { element.copy_attribute_value(&attribute, NonNull::from(&mut value)) };
    if error != AXError::Success || value.is_null() {
        return None;
    }
    Some(unsafe { CFRetained::from_raw(NonNull::new(value as *mut CFType).unwrap()) })
}

fn copy_ax_rect_value(
    attribute_value: &CFType,
    kind: AXValueType,
    target: NonNull<std::ffi::c_void>,
) -> bool {
    let Some(value) = attribute_value.downcast_ref::<AXValue>() else {
        return false;
    };
    unsafe { value.value(kind, target) }
}

/// Layer 1: `AXBoundsForRange` on the focused element; layer 2: its frame.
/// Returns the caret bounds in AppKit coordinates, or `None` for the mouse
/// fallback. The caller runs this on the main thread with 0.2 s AX timeouts.
fn caret_rect(mtm: MainThreadMarker) -> Option<(f64, f64, f64, f64)> {
    let frames = screen_frames(mtm);
    let primary = frames
        .first()
        .copied()
        .or_else(|| NSScreen::mainScreen(mtm).map(|screen| screen.frame()))?;
    let application = NSWorkspace::sharedWorkspace().frontmostApplication()?;

    unsafe {
        let app_element = AXUIElement::new_application(application.processIdentifier());
        app_element.set_messaging_timeout(AX_CARET_TIMEOUT_SECONDS);
        let focused = copy_ax_attribute(&app_element, "AXFocusedUIElement")?;
        let focused: CFRetained<AXUIElement> = CFRetained::cast_unchecked(focused);
        focused.set_messaging_timeout(AX_CARET_TIMEOUT_SECONDS);

        if let Some(selected_range) = copy_ax_attribute(&focused, "AXSelectedTextRange") {
            let attribute = CFString::from_static_str("AXBoundsForRange");
            let mut result: *const CFType = std::ptr::null();
            let error = focused.copy_parameterized_attribute_value(
                &attribute,
                &selected_range,
                NonNull::from(&mut result),
            );
            if error == AXError::Success && !result.is_null() {
                let bounds = CFRetained::from_raw(NonNull::new(result as *mut CFType).unwrap());
                let mut rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0));
                if copy_ax_rect_value(
                    &bounds,
                    AXValueType::CGRect,
                    NonNull::from(&mut rect).cast(),
                ) && rect.size.height > 0.0
                    && ax_rect_in_any_screen(
                        &frames,
                        primary,
                        rect.origin.x,
                        rect.origin.y,
                        rect.size.width,
                        rect.size.height,
                    )
                {
                    return Some(flip_ax_rect(
                        primary,
                        rect.origin.x,
                        rect.origin.y,
                        rect.size.width.max(2.0),
                        rect.size.height,
                    ));
                }
            }
        }

        let position = copy_ax_attribute(&focused, "AXPosition")?;
        let size = copy_ax_attribute(&focused, "AXSize")?;
        let mut point = CGPoint::new(0.0, 0.0);
        let mut extent = CGSize::new(0.0, 0.0);
        if !copy_ax_rect_value(
            &position,
            AXValueType::CGPoint,
            NonNull::from(&mut point).cast(),
        ) || !copy_ax_rect_value(
            &size,
            AXValueType::CGSize,
            NonNull::from(&mut extent).cast(),
        ) || !ax_rect_in_any_screen(
            &frames,
            primary,
            point.x,
            point.y,
            extent.width,
            extent.height,
        ) {
            return None;
        }
        Some(flip_ax_rect(
            primary,
            point.x,
            point.y,
            extent.width,
            extent.height,
        ))
    }
}

struct WaveformIvars {
    phase: Cell<f64>,
    level: Cell<f64>,
    target_level: Cell<f64>,
    reduce_motion: Cell<bool>,
    colors: Vec<Retained<NSColor>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = WaveformIvars]
    struct WaveformView;

    unsafe impl NSObjectProtocol for WaveformView {}

    impl WaveformView {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: objc2::rc::Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let colors = (0..BAR_COUNT)
                .map(|index| {
                    NSColor::colorWithCalibratedHue_saturation_brightness_alpha(
                        0.48 + index as f64 * 0.007,
                        0.40,
                        0.93,
                        0.98,
                    )
                })
                .collect();
            let this = this.set_ivars(WaveformIvars {
                phase: Cell::new(0.0),
                level: Cell::new(0.04),
                target_level: Cell::new(0.04),
                reduce_motion: Cell::new(false),
                colors,
            });
            let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            view.setWantsLayer(true);
            view
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            false
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let bounds = self.bounds();
            let radius = bounds.size.height / 2.0;
            let background =
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds, radius, radius);
            NSColor::colorWithCalibratedWhite_alpha(0.065, 0.94).setFill();
            background.fill();
            NSColor::colorWithCalibratedWhite_alpha(1.0, 0.12).setStroke();
            background.setLineWidth(1.0);
            background.stroke();

            let ivars = self.ivars();
            let level = ivars.level.get();
            let phase = ivars.phase.get();
            let bar_width = 3.2;
            let gap = 4.0;
            let total_width = BAR_COUNT as f64 * bar_width + (BAR_COUNT as f64 - 1.0) * gap;
            let start_x = (bounds.size.width - total_width) / 2.0;
            let center_y = 14.0;

            for index in 0..BAR_COUNT {
                let distance =
                    (index as f64 - (BAR_COUNT as f64 - 1.0) / 2.0).abs() / (BAR_COUNT as f64 / 2.0);
                let envelope = (1.0 - distance * 0.58).max(0.30);
                let motion =
                    0.62 + 0.38 * (phase + index as f64 * 0.72).sin().powi(2);
                let secondary = 0.82 + 0.18 * (phase * 0.61 - index as f64 * 0.31).sin();
                let height = (3.5 + 19.0 * level * envelope * motion * secondary).clamp(3.5, 23.0);
                let x = start_x + index as f64 * (bar_width + gap);
                let rect = NSRect::new(
                    NSPoint::new(x, center_y - height / 2.0),
                    NSSize::new(bar_width, height),
                );
                let bar = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    rect,
                    bar_width / 2.0,
                    bar_width / 2.0,
                );
                ivars.colors[index].setFill();
                bar.fill();
            }
        }
    }
);

impl WaveformView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT));
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }

    fn set_audio_level(&self, level: f64) {
        self.ivars().target_level.set(level.clamp(0.025, 1.0));
        if self.ivars().reduce_motion.get() {
            self.ivars().level.set(self.ivars().target_level.get());
            self.setNeedsDisplay(true);
        }
    }

    fn reset_motion(&self, reduced: bool) {
        self.ivars().reduce_motion.set(reduced);
        self.ivars().phase.set(0.0);
        self.ivars().level.set(0.04);
        self.ivars().target_level.set(0.04);
        self.setNeedsDisplay(true);
    }

    fn advance(&self) {
        if self.ivars().reduce_motion.get() {
            return;
        }
        let phase = self.ivars().phase.get() + 0.18;
        self.ivars().phase.set(phase);
        let level = self.ivars().level.get();
        let target = self.ivars().target_level.get();
        let rate = if target > level { 0.58 } else { 0.16 };
        self.ivars().level.set(level + (target - level) * rate);
        self.ivars()
            .target_level
            .set((target * 0.82).max(0.025));
        self.setNeedsDisplay(true);
    }
}

struct Fade {
    from: f64,
    to: f64,
    started: Instant,
    duration: Duration,
}

pub struct PreviewWindow {
    panel: Option<Retained<NSPanel>>,
    waveform: Option<Retained<WaveformView>>,
    label: Option<Retained<NSTextField>>,
    generation: u64,
    visible: bool,
    last_text: String,
    reduced_motion: bool,
    animating: bool,
    fade: Option<Fade>,
    notice_deadline: Option<(u64, Instant)>,
}

impl PreviewWindow {
    pub fn new() -> Self {
        Self {
            panel: None,
            waveform: None,
            label: None,
            generation: 0,
            visible: false,
            last_text: String::new(),
            reduced_motion: false,
            animating: false,
            fade: None,
            notice_deadline: None,
        }
    }

    /// Mirror `show`: a fresh panel fades in; a visible one just refreshes.
    pub fn show(&mut self, mtm: MainThreadMarker, preparing: bool) {
        self.show_now(mtm);
        if preparing {
            self.stop_animation();
            self.set_label("准备录音…");
        }
    }

    pub fn show_notice(&mut self, mtm: MainThreadMarker, message: &str) {
        self.show_now(mtm);
        self.stop_animation();
        self.set_label(message);
        let generation = self.generation;
        self.notice_deadline =
            Some((generation, Instant::now() + Duration::from_secs_f64(NOTICE_SECONDS)));
    }

    pub fn hide(&mut self) {
        if self.panel.is_none() {
            return;
        }
        self.generation += 1;
        self.visible = false;
        self.animating = false;
        self.notice_deadline = None;
        let duration = if self.reduced_motion {
            Duration::ZERO
        } else {
            Duration::from_millis(180)
        };
        self.start_fade(0.0, duration);
    }

    pub fn set_text(&mut self, text: &str) {
        let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized != self.last_text {
            self.set_label(&normalized);
        }
    }

    pub fn set_level(&self, level: f64) {
        if let Some(waveform) = self.waveform.as_ref() {
            waveform.set_audio_level(level);
        }
    }

    /// The microphone stopped; freeze the wave without hiding the caption.
    pub fn set_processing(&mut self) {
        self.animating = false;
        if let Some(waveform) = self.waveform.as_ref() {
            waveform.reset_motion(true);
        }
    }

    /// 30 Hz UI pump: fade progress, notice expiry, wave animation.
    pub fn tick(&mut self, mtm: MainThreadMarker) {
        if let Some((generation, deadline)) = self.notice_deadline {
            if generation == self.generation && Instant::now() >= deadline {
                self.hide();
            }
        }
        if let Some(fade) = self.fade.as_ref() {
            let Some(panel) = self.panel.as_ref() else {
                return;
            };
            let elapsed = fade.started.elapsed();
            let progress = if fade.duration.is_zero() {
                1.0
            } else {
                (elapsed.as_secs_f64() / fade.duration.as_secs_f64()).min(1.0)
            };
            let alpha = fade.from + (fade.to - fade.from) * progress;
            panel.setAlphaValue(alpha);
            if progress >= 1.0 {
                let hidden = fade.to == 0.0;
                self.fade = None;
                if hidden && !self.visible {
                    panel.orderOut(None);
                }
            }
        }
        if self.visible && self.animating {
            if let Some(waveform) = self.waveform.as_ref() {
                waveform.advance();
            }
        }
        let _ = mtm;
    }

    fn show_now(&mut self, mtm: MainThreadMarker) {
        self.ensure_panel(mtm);
        self.generation += 1;
        if caret_follow_enabled() {
            self.position_near_caret(mtm);
        } else {
            self.position_fixed(mtm);
        }
        self.set_label("");
        self.reduced_motion = reduce_motion();
        if let Some(waveform) = self.waveform.as_ref() {
            waveform.reset_motion(self.reduced_motion);
        }
        self.animating = !self.reduced_motion;
        self.notice_deadline = None;
        let was_visible = self.visible;
        if let Some(panel) = self.panel.as_ref() {
            panel.setAlphaValue(if was_visible { 1.0 } else { 0.0 });
            panel.orderFrontRegardless();
        }
        self.visible = true;
        if !was_visible {
            let duration = if self.reduced_motion {
                Duration::ZERO
            } else {
                Duration::from_millis(60)
            };
            self.start_fade(1.0, duration);
        }
    }

    fn ensure_panel(&mut self, mtm: MainThreadMarker) {
        if self.panel.is_some() {
            return;
        }
        let screen = NSScreen::mainScreen(mtm).map(|screen| screen.visibleFrame());
        let frame = match screen {
            Some(frame) => NSRect::new(
                NSPoint::new(
                    frame.origin.x + (frame.size.width - WIDTH) / 2.0,
                    frame.origin.y + frame.size.height - 140.0,
                ),
                NSSize::new(WIDTH, HEIGHT),
            ),
            None => NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT)),
        };
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setLevel(NSPopUpMenuWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        panel.setHidesOnDeactivate(false);
        panel.setCanHide(false);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(true);
        panel.setIgnoresMouseEvents(true);
        panel.setAlphaValue(0.0);

        let waveform = WaveformView::new(mtm);
        let label = NSTextField::labelWithString(ns_string!(""), mtm);
        label.setFrame(NSRect::new(
            NSPoint::new(16.0, 32.0),
            NSSize::new(WIDTH - 32.0, 18.0),
        ));
        label.setAlignment(NSTextAlignment(1));
        label.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, 0.25)));
        label.setTextColor(Some(&NSColor::whiteColor().colorWithAlphaComponent(0.92)));
        if let Some(cell) = label.cell() {
            cell.setUsesSingleLineMode(true);
            cell.setLineBreakMode(NSLineBreakMode::ByTruncatingHead);
        }
        waveform.addSubview(&label);
        panel.setContentView(Some(&waveform));

        self.panel = Some(panel);
        self.waveform = Some(waveform);
        self.label = Some(label);
    }

    fn position_fixed(&self, mtm: MainThreadMarker) {
        let Some(panel) = self.panel.as_ref() else {
            return;
        };
        let Some(screen) = NSScreen::mainScreen(mtm) else {
            return;
        };
        let frame = screen.visibleFrame();
        let x = frame.origin.x + (frame.size.width - WIDTH) / 2.0;
        panel.setFrame_display(
            NSRect::new(
                NSPoint::new(x, frame.origin.y + FIXED_BOTTOM_OFFSET),
                NSSize::new(WIDTH, HEIGHT),
            ),
            true,
        );
    }

    fn position_near_caret(&self, mtm: MainThreadMarker) {
        let Some(panel) = self.panel.as_ref() else {
            return;
        };
        let frames = screen_frames(mtm);
        let near_caret = caret_rect(mtm).and_then(|(caret_x, caret_y, _caret_width, caret_height)| {
            let visible = visible_frame_for_point(mtm, &frames, caret_x, caret_y)?;
            if caret_height > visible.size.height {
                return None;
            }
            let mut y = caret_y - HEIGHT - CARET_GAP;
            if y < visible.origin.y + CARET_GAP {
                y = caret_y + caret_height + CARET_GAP;
            }
            Some(clamp_to_frame(
                caret_x,
                y,
                WIDTH,
                HEIGHT,
                visible,
                FRAME_MARGIN,
            ))
        });
        let Some((x, y)) = near_caret.or_else(|| mouse_position(mtm, &frames)) else {
            return;
        };
        panel.setFrame_display(
            NSRect::new(NSPoint::new(x, y), NSSize::new(WIDTH, HEIGHT)),
            true,
        );
    }

    fn set_label(&mut self, text: &str) {
        if let Some(label) = self.label.as_ref() {
            label.setStringValue(&objc2_foundation::NSString::from_str(text));
        }
        self.last_text = text.to_string();
    }

    fn start_fade(&mut self, to: f64, duration: Duration) {
        let from = self
            .panel
            .as_ref()
            .map(|panel| panel.alphaValue())
            .unwrap_or(to);
        self.fade = Some(Fade {
            from,
            to,
            started: Instant::now(),
            duration,
        });
        if let Some(panel) = self.panel.as_ref() {
            panel.setAlphaValue(from);
        }
    }

    fn stop_animation(&mut self) {
        self.animating = false;
    }
}

impl Default for PreviewWindow {
    fn default() -> Self {
        Self::new()
    }
}

fn reduce_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// Last-resort anchor when AX cannot report a caret: the mouse pointer.
fn mouse_position(mtm: MainThreadMarker, frames: &[NSRect]) -> Option<(f64, f64)> {
    let mouse = NSEvent::mouseLocation();
    let visible = visible_frame_for_point(mtm, frames, mouse.x, mouse.y)?;
    let x = mouse.x - WIDTH / 2.0;
    let y = mouse.y - HEIGHT - 18.0;
    Some(clamp_to_frame(x, y, WIDTH, HEIGHT, visible, FRAME_MARGIN))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
    }

    #[test]
    fn clamping_keeps_the_panel_inside_the_visible_frame() {
        let frame = rect(0.0, 0.0, 1000.0, 800.0);
        assert_eq!(clamp_to_frame(-50.0, -50.0, WIDTH, HEIGHT, frame, 10.0), (10.0, 10.0));
        assert_eq!(
            clamp_to_frame(5000.0, 5000.0, WIDTH, HEIGHT, frame, 10.0),
            (1000.0 - WIDTH - 10.0, 800.0 - HEIGHT - 10.0)
        );
        assert_eq!(clamp_to_frame(100.0, 200.0, WIDTH, HEIGHT, frame, 10.0), (100.0, 200.0));
    }

    #[test]
    fn ax_rects_flip_to_appkit_coordinates() {
        let primary = rect(0.0, 0.0, 1920.0, 1080.0);
        assert_eq!(flip_ax_rect(primary, 100.0, 50.0, 2.0, 20.0), (100.0, 1010.0, 2.0, 20.0));
    }

    #[test]
    fn bogus_ax_rects_fall_outside_every_screen() {
        let primary = rect(0.0, 0.0, 1920.0, 1080.0);
        let secondary = rect(1920.0, 0.0, 1280.0, 800.0);
        let frames = [primary, secondary];
        assert!(ax_rect_in_any_screen(&frames, primary, 100.0, 100.0, 2.0, 18.0));
        // Negative AX coordinates are what Electron reports on Tahoe.
        assert!(!ax_rect_in_any_screen(&frames, primary, -4000.0, 100.0, 2.0, 18.0));
        // Zero-height rects must never be trusted (handled by the caller).
        // The x coordinate is what rejects a stale primary-screen rect here.
        assert!(!ax_rect_in_any_screen(&frames, primary, 2000.0, 100.0, 10.0, 18.0));
        // A real caret on the secondary display still passes.
        assert!(ax_rect_in_any_screen(&frames, primary, 2000.0, 700.0, 10.0, 18.0));
    }

    #[test]
    fn screens_pick_the_containing_frame_then_the_nearest() {
        let primary = rect(0.0, 0.0, 1920.0, 1080.0);
        let secondary = rect(1920.0, 0.0, 1280.0, 800.0);
        let frames = [primary, secondary];
        assert_eq!(screen_index_for_point(&frames, 100.0, 100.0), Some(0));
        assert_eq!(screen_index_for_point(&frames, 2000.0, 400.0), Some(1));
        assert_eq!(screen_index_for_point(&frames, 5000.0, 2000.0), Some(1));
        assert_eq!(screen_index_for_point(&[], 0.0, 0.0), None);
        assert!(frame_contains(primary, 0.0, 0.0));
        assert!(!frame_contains(primary, -1.0, 0.0));
    }
}
