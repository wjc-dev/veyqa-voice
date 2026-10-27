use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyMode {
    Hold,
    Toggle,
}

impl HotkeyMode {
    pub fn from_config(value: &str) -> Self {
        if value.trim().eq_ignore_ascii_case("toggle") {
            Self::Toggle
        } else {
            Self::Hold
        }
    }
}

pub trait GestureSink {
    /// Returns whether recording actually started (shortcut capture or an
    /// existing session can reject the start).
    fn start(&mut self) -> bool;
    fn stop(&mut self);
    fn cancel(&mut self);
    fn toggle(&mut self);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerRequest {
    pub generation: u64,
    pub delay: Duration,
}

pub struct ModifierGesture {
    mode: HotkeyMode,
    hold_delay: Duration,
    down: bool,
    blocked: bool,
    started: bool,
    generation: u64,
}

impl ModifierGesture {
    pub const DEFAULT_HOLD_DELAY: Duration = Duration::from_millis(180);
    pub const RIGHT_OPTION_HOLD_DELAY: Duration = Duration::from_millis(80);

    pub fn new(mode: HotkeyMode, hold_delay: Duration) -> Self {
        Self {
            mode,
            hold_delay,
            down: false,
            blocked: false,
            started: false,
            generation: 0,
        }
    }

    pub fn set_mode(&mut self, mode: HotkeyMode) {
        self.mode = mode;
    }

    pub fn is_down(&self) -> bool {
        self.down
    }

    pub fn is_started(&self) -> bool {
        self.started
    }

    pub fn press(&mut self, blocked: bool) -> Option<TimerRequest> {
        if self.down {
            return None;
        }
        self.down = true;
        self.blocked = blocked;
        self.started = false;
        if blocked || self.mode != HotkeyMode::Hold {
            return None;
        }
        self.generation += 1;
        Some(TimerRequest {
            generation: self.generation,
            delay: self.hold_delay,
        })
    }

    pub fn chord<S: GestureSink + ?Sized>(&mut self, sink: &mut S) {
        if !self.down {
            return;
        }
        self.blocked = true;
        self.cancel_timer();
        if self.started {
            self.started = false;
            sink.cancel();
        }
    }

    pub fn release<S: GestureSink + ?Sized>(&mut self, sink: &mut S, confirmed: bool) {
        if !self.down {
            return;
        }
        self.cancel_timer();
        self.down = false;
        if self.started {
            self.started = false;
            sink.stop();
        } else if self.mode == HotkeyMode::Toggle && !self.blocked && confirmed {
            sink.toggle();
        }
        self.blocked = false;
    }

    pub fn on_timer<S: GestureSink + ?Sized>(&mut self, generation: u64, sink: &mut S) -> bool {
        if generation != self.generation || !self.down || self.blocked {
            return false;
        }
        self.started = sink.start();
        self.started
    }

    pub fn reset<S: GestureSink + ?Sized>(&mut self, sink: &mut S) {
        self.cancel_timer();
        self.down = false;
        self.blocked = false;
        if self.started {
            self.started = false;
            sink.cancel();
        }
    }

    fn cancel_timer(&mut self) {
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingSink {
        actions: Vec<&'static str>,
        recording: bool,
        deny_start: bool,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                actions: Vec::new(),
                recording: false,
                deny_start: false,
            }
        }

        fn assert_actions(&self, expected: &[&'static str]) {
            assert_eq!(self.actions, expected);
        }
    }

    impl GestureSink for RecordingSink {
        fn start(&mut self) -> bool {
            if self.deny_start {
                return false;
            }
            self.actions.push("start");
            self.recording = true;
            true
        }

        fn stop(&mut self) {
            self.actions.push("stop");
            self.recording = false;
        }

        fn cancel(&mut self) {
            self.actions.push("cancel");
            self.recording = false;
        }

        fn toggle(&mut self) {
            self.actions.push("toggle");
            self.recording = !self.recording;
        }
    }

    fn hold_gesture() -> ModifierGesture {
        ModifierGesture::new(HotkeyMode::Hold, ModifierGesture::DEFAULT_HOLD_DELAY)
    }

    fn toggle_gesture() -> ModifierGesture {
        ModifierGesture::new(HotkeyMode::Toggle, ModifierGesture::DEFAULT_HOLD_DELAY)
    }

    #[test]
    fn hold_starts_on_timer_and_stops_on_release() {
        let mut gesture = hold_gesture();
        let mut sink = RecordingSink::new();
        let timer = gesture.press(false).expect("timer");
        assert_eq!(timer.delay, Duration::from_millis(180));
        sink.assert_actions(&[]);
        assert!(gesture.on_timer(timer.generation, &mut sink));
        gesture.release(&mut sink, true);
        sink.assert_actions(&["start", "stop"]);
    }

    #[test]
    fn toggle_toggles_once_per_press_release() {
        let mut gesture = toggle_gesture();
        let mut sink = RecordingSink::new();
        assert!(gesture.press(false).is_none());
        gesture.release(&mut sink, true);
        gesture.release(&mut sink, true);
        sink.assert_actions(&["toggle"]);
    }

    #[test]
    fn duplicate_press_does_not_reset_the_gesture() {
        let mut gesture = toggle_gesture();
        let mut sink = RecordingSink::new();
        gesture.press(false);
        gesture.press(false);
        gesture.release(&mut sink, true);
        sink.assert_actions(&["toggle"]);
    }

    #[test]
    fn chord_before_threshold_never_starts() {
        let mut gesture = hold_gesture();
        let mut sink = RecordingSink::new();
        let timer = gesture.press(false).expect("timer");
        gesture.chord(&mut sink);
        assert!(!gesture.on_timer(timer.generation, &mut sink));
        gesture.release(&mut sink, true);
        sink.assert_actions(&[]);
    }

    #[test]
    fn late_chord_cancels_started_hold() {
        let mut gesture = hold_gesture();
        let mut sink = RecordingSink::new();
        let timer = gesture.press(false).expect("timer");
        assert!(gesture.on_timer(timer.generation, &mut sink));
        gesture.chord(&mut sink);
        gesture.release(&mut sink, true);
        sink.assert_actions(&["start", "cancel"]);
    }

    #[test]
    fn stale_timer_cannot_start_a_later_gesture() {
        let mut gesture = hold_gesture();
        let mut sink = RecordingSink::new();
        let first = gesture.press(false).expect("timer");
        gesture.release(&mut sink, true);
        let second = gesture.press(false).expect("timer");
        assert!(!gesture.on_timer(first.generation, &mut sink));
        assert!(gesture.on_timer(second.generation, &mut sink));
        gesture.release(&mut sink, true);
        sink.assert_actions(&["start", "stop"]);
    }

    #[test]
    fn denied_start_does_not_commit_text() {
        let mut gesture = hold_gesture();
        let mut sink = RecordingSink::new();
        sink.deny_start = true;
        let timer = gesture.press(false).expect("timer");
        assert!(!gesture.on_timer(timer.generation, &mut sink));
        gesture.release(&mut sink, true);
        sink.assert_actions(&[]);
    }

    #[test]
    fn blocked_press_has_no_timer_and_no_toggle() {
        let mut hold = hold_gesture();
        let mut sink = RecordingSink::new();
        assert!(hold.press(true).is_none());
        hold.release(&mut sink, true);
        sink.assert_actions(&[]);

        let mut toggle = toggle_gesture();
        toggle.press(true);
        toggle.release(&mut sink, true);
        sink.assert_actions(&[]);
    }

    #[test]
    fn reset_cancels_pending_timer_and_active_recording() {
        let mut gesture = hold_gesture();
        let mut sink = RecordingSink::new();
        let timer = gesture.press(false).expect("timer");
        gesture.reset(&mut sink);
        assert!(!gesture.on_timer(timer.generation, &mut sink));

        let timer = gesture.press(false).expect("timer");
        assert!(gesture.on_timer(timer.generation, &mut sink));
        gesture.reset(&mut sink);
        sink.assert_actions(&["start", "cancel"]);
    }

    #[test]
    fn unconfirmed_release_does_not_toggle() {
        let mut gesture = toggle_gesture();
        let mut sink = RecordingSink::new();
        gesture.press(false);
        gesture.release(&mut sink, false);
        sink.assert_actions(&[]);
    }
}
