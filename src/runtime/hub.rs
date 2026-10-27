//! Shared input-state, transition generations, and health timestamps.
//!
//! Replaces Python's `KeyboardManager` locks together with the generation
//! counter used to drop a cancelled transition.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use crate::state::InputState;

struct InputSnapshot {
    state: InputState,
    since: f64,
}

#[derive(Default)]
struct Gate {
    recording: bool,
    cancel_pending: bool,
}

pub struct StateHub {
    input: Mutex<InputSnapshot>,
    gate: Mutex<Gate>,
    generation: AtomicU64,
    active_generation: AtomicU64,
    transition_started: Mutex<Option<f64>>,
    listener_poll: Mutex<f64>,
    engine_busy: AtomicBool,
    session_active: AtomicBool,
    finished_session: Mutex<Option<String>>,
    microphone_test_active: AtomicBool,
}

impl StateHub {
    pub fn new(now: f64) -> Self {
        Self {
            input: Mutex::new(InputSnapshot {
                state: InputState::Idle,
                since: now,
            }),
            gate: Mutex::new(Gate::default()),
            generation: AtomicU64::new(0),
            active_generation: AtomicU64::new(0),
            transition_started: Mutex::new(None),
            listener_poll: Mutex::new(now),
            engine_busy: AtomicBool::new(false),
            session_active: AtomicBool::new(false),
            finished_session: Mutex::new(None),
            microphone_test_active: AtomicBool::new(false),
        }
    }

    pub fn state(&self) -> InputState {
        self.input.lock().unwrap().state
    }

    pub fn state_since(&self) -> f64 {
        self.input.lock().unwrap().since
    }

    pub fn state_for(&self, now: f64) -> (InputState, f64) {
        let guard = self.input.lock().unwrap();
        (guard.state, now - guard.since)
    }

    pub fn set_state(&self, state: InputState, now: f64) {
        let mut guard = self.input.lock().unwrap();
        if guard.state == state {
            return;
        }
        guard.state = state;
        guard.since = now;
    }

    /// Warning/error states clear themselves after two seconds, like Python.
    pub fn reset_message_state(&self, now: f64) {
        let mut guard = self.input.lock().unwrap();
        if matches!(guard.state, InputState::Warning | InputState::Error) {
            guard.state = InputState::Idle;
            guard.since = now;
        }
    }

    /// `KeyboardManager.reset_state`: drop the latch and return to idle.
    pub fn reset_keyboard(&self, now: f64) {
        {
            let mut gate = self.gate.lock().unwrap();
            gate.recording = false;
            gate.cancel_pending = false;
        }
        self.set_state(InputState::Idle, now);
    }

    /// Returns a fresh generation when a new utterance may start.
    pub fn try_begin_recording(&self, now: f64) -> Option<u64> {
        if self.transition_started_at().is_some() {
            return None;
        }
        let mut gate = self.gate.lock().unwrap();
        if gate.recording || gate.cancel_pending {
            return None;
        }
        let mut input = self.input.lock().unwrap();
        if !input.state.can_start_recording() {
            return None;
        }
        gate.recording = true;
        input.state = InputState::Recording;
        input.since = now;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        Some(generation)
    }

    pub fn is_recording(&self) -> bool {
        self.gate.lock().unwrap().recording
    }

    pub fn end_recording(&self) -> bool {
        let mut gate = self.gate.lock().unwrap();
        if !gate.recording {
            return false;
        }
        gate.recording = false;
        true
    }

    /// A cancel invalidates the in-flight start and blocks immediate restarts
    /// until the actor has aborted the device.
    pub fn begin_cancel(&self) -> Option<u64> {
        let mut gate = self.gate.lock().unwrap();
        if !gate.recording {
            return None;
        }
        gate.recording = false;
        gate.cancel_pending = true;
        Some(self.generation.fetch_add(1, Ordering::SeqCst) + 1)
    }

    pub fn finish_cancel(&self) {
        self.gate.lock().unwrap().cancel_pending = false;
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn transition_cancelled(&self, generation: u64) -> bool {
        self.generation() != generation
    }

    pub fn set_active_generation(&self, generation: u64) {
        self.active_generation.store(generation, Ordering::SeqCst);
    }

    pub fn active_generation(&self) -> u64 {
        self.active_generation.load(Ordering::SeqCst)
    }

    pub fn begin_transition(&self, now: f64) {
        *self.transition_started.lock().unwrap() = Some(now);
    }

    pub fn end_transition(&self) {
        *self.transition_started.lock().unwrap() = None;
    }

    pub fn transition_started_at(&self) -> Option<f64> {
        *self.transition_started.lock().unwrap()
    }

    pub fn mark_listener_poll(&self, now: f64) {
        *self.listener_poll.lock().unwrap() = now;
    }

    pub fn listener_lag(&self, now: f64) -> f64 {
        now - *self.listener_poll.lock().unwrap()
    }

    /// The actor mirrors `session_active || provider_alive` here so the health
    /// tick can decide whether a settings save has to wait.
    pub fn set_engine_busy(&self, busy: bool) {
        self.engine_busy.store(busy, Ordering::SeqCst);
    }

    pub fn engine_busy(&self) -> bool {
        self.engine_busy.load(Ordering::SeqCst)
    }

    pub fn set_microphone_test_active(&self, active: bool) {
        self.microphone_test_active.store(active, Ordering::SeqCst);
    }

    pub fn microphone_test_active(&self) -> bool {
        self.microphone_test_active.load(Ordering::SeqCst)
    }

    /// Mirrors Python's `_current_session is not None` for the notice gate.
    pub fn set_session_active(&self, active: bool) {
        self.session_active.store(active, Ordering::SeqCst);
    }

    pub fn session_active(&self) -> bool {
        self.session_active.load(Ordering::SeqCst)
    }

    pub fn note_finished_session(&self, session_id: String) {
        *self.finished_session.lock().unwrap() = Some(session_id);
    }

    pub fn finished_session_id(&self) -> Option<String> {
        self.finished_session.lock().unwrap().clone()
    }

    /// Python's `_update_busy`: a session, a held key, or a slow transition.
    pub fn busy(&self) -> bool {
        self.engine_busy()
            || self.is_recording()
            || self.microphone_test_active()
            || self.transition_started_at().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_once_and_needs_a_matching_generation() {
        let hub = StateHub::new(0.0);
        let generation = hub.try_begin_recording(1.0).unwrap();
        assert_eq!(hub.state(), InputState::Recording);
        assert!(hub.try_begin_recording(1.1).is_none());
        assert!(!hub.transition_cancelled(generation));
        let cancelled = hub.begin_cancel().unwrap();
        assert!(cancelled > generation);
        assert!(hub.transition_cancelled(generation));
        assert!(hub.try_begin_recording(1.2).is_none());
        hub.finish_cancel();
        hub.reset_keyboard(1.3);
        assert_eq!(hub.state(), InputState::Idle);
        assert!(hub.try_begin_recording(1.4).is_some());
    }

    #[test]
    fn a_start_is_refused_while_a_transition_is_running() {
        let hub = StateHub::new(0.0);
        hub.begin_transition(1.0);
        assert!(hub.try_begin_recording(1.0).is_none());
        hub.end_transition();
        assert!(hub.try_begin_recording(1.0).is_some());
    }

    #[test]
    fn busy_tracks_engine_keys_and_transitions() {
        let hub = StateHub::new(0.0);
        assert!(!hub.busy());
        hub.set_engine_busy(true);
        assert!(hub.busy());
        hub.set_engine_busy(false);
        hub.try_begin_recording(0.0).unwrap();
        assert!(hub.busy());
        hub.end_recording();
        assert!(!hub.busy());
        hub.begin_transition(0.0);
        assert!(hub.busy());
        hub.end_transition();
        assert!(!hub.busy());
        hub.set_microphone_test_active(true);
        assert!(hub.busy());
        hub.set_microphone_test_active(false);
        assert!(!hub.busy());
    }

    #[test]
    fn warning_and_error_clear_but_processing_does_not() {
        let hub = StateHub::new(0.0);
        hub.set_state(InputState::Warning, 1.0);
        hub.reset_message_state(2.0);
        assert_eq!(hub.state(), InputState::Idle);

        hub.set_state(InputState::Processing, 3.0);
        hub.reset_message_state(4.0);
        assert_eq!(hub.state(), InputState::Processing);
        assert_eq!(hub.state_for(4.5), (InputState::Processing, 1.5));
    }

    #[test]
    fn a_finished_recording_does_not_block_the_next_stop() {
        let hub = StateHub::new(0.0);
        assert!(hub.try_begin_recording(0.0).is_some());
        assert!(hub.end_recording());
        assert!(!hub.end_recording());
    }

    #[test]
    fn listener_lag_tracks_the_last_poll() {
        let hub = StateHub::new(10.0);
        assert_eq!(hub.listener_lag(11.0), 1.0);
        hub.mark_listener_poll(15.0);
        assert_eq!(hub.listener_lag(16.0), 1.0);
    }

    #[test]
    fn session_lifecycle_is_visible_to_the_notice_gate() {
        let hub = StateHub::new(0.0);
        assert!(!hub.session_active());
        assert_eq!(hub.finished_session_id(), None);
        hub.set_session_active(true);
        assert!(hub.session_active());
        hub.note_finished_session("session-a".into());
        hub.set_session_active(false);
        assert!(!hub.session_active());
        assert_eq!(hub.finished_session_id().as_deref(), Some("session-a"));
        hub.note_finished_session("session-b".into());
        assert_eq!(hub.finished_session_id().as_deref(), Some("session-b"));
    }
}
