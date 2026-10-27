//! Hotkey event actor: one thread owns the gesture state machine.
//!
//! The CGEventTap callback only forwards raw events. This actor applies the
//! same rules as `KeyboardManager._darwin_intercept`: Esc cancels, settings
//! shortcut capture suppresses the shortcut, hold timers fire here, and a
//! lost key-up is confirmed by two poll observations.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{
    bounded, Receiver, RecvTimeoutError, Sender,
};
use objc2_core_graphics::{CGEventSource, CGEventSourceStateID};

use super::carbon::CarbonHotkey;
use super::gesture::{GestureSink, HotkeyMode, TimerRequest};
use super::monitor::GlobalMonitor;
use super::tap::{self, TapHandle, TapMessage};
use super::{
    shortcut_capture_pid, HotkeyHandler, HotkeySpec, RawEventType, ESCAPE_KEYCODE,
};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const CAPTURE_CACHE_TTL: Duration = Duration::from_millis(150);

pub trait RecordingSink: Send {
    fn start(&mut self) -> bool;
    fn stop(&mut self);
    fn cancel(&mut self);
    fn toggle(&mut self);
    fn is_recording(&self) -> bool;
}

pub trait KeyStateSource: Send {
    fn key_down(&mut self, vk: i64) -> bool;
    fn flags(&mut self) -> u64;
}

pub struct SystemKeyState;

impl KeyStateSource for SystemKeyState {
    fn key_down(&mut self, vk: i64) -> bool {
        CGEventSource::key_state(CGEventSourceStateID::CombinedSessionState, vk as u16)
    }

    fn flags(&mut self) -> u64 {
        CGEventSource::flags_state(CGEventSourceStateID::CombinedSessionState).0
    }
}

pub struct ShortcutCapture {
    path: PathBuf,
    checked_at: Option<Instant>,
    active: bool,
}

impl ShortcutCapture {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            checked_at: None,
            active: false,
        }
    }

    pub fn is_active(&mut self, force: bool) -> bool {
        let now = Instant::now();
        if !force
            && self
                .checked_at
                .is_some_and(|checked| now.duration_since(checked) < CAPTURE_CACHE_TTL)
        {
            return self.active;
        }
        self.checked_at = Some(now);
        let active = fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| shortcut_capture_pid(&text))
            .is_some_and(process_alive);
        if !active {
            let _ = fs::remove_file(&self.path);
        }
        self.active = active;
        active
    }
}

fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

struct SinkRef<'a> {
    sink: &'a mut dyn RecordingSink,
    capture: &'a mut ShortcutCapture,
}

impl GestureSink for SinkRef<'_> {
    fn start(&mut self) -> bool {
        if self.capture.is_active(true) {
            return false;
        }
        self.sink.start()
    }

    fn stop(&mut self) {
        self.sink.stop();
    }

    fn cancel(&mut self) {
        self.sink.cancel();
    }

    fn toggle(&mut self) {
        self.sink.toggle();
    }
}

/// Reconciliation polls bypass the settings shortcut-capture guard: they only
/// close an already-open gesture.
impl GestureSink for dyn RecordingSink + '_ {
    fn start(&mut self) -> bool {
        RecordingSink::start(self)
    }

    fn stop(&mut self) {
        RecordingSink::stop(self);
    }

    fn cancel(&mut self) {
        RecordingSink::cancel(self);
    }

    fn toggle(&mut self) {
        RecordingSink::toggle(self);
    }
}

struct PendingTimer {
    request: TimerRequest,
    deadline: Instant,
}

/// The health tick treats a stalled actor as a dead hotkey loop.
#[derive(Clone)]
pub struct ListenerHeartbeat {
    last: Arc<Mutex<Instant>>,
    available: Arc<AtomicBool>,
}

impl ListenerHeartbeat {
    fn new(available: Arc<AtomicBool>) -> Self {
        Self {
            last: Arc::new(Mutex::new(Instant::now())),
            available,
        }
    }

    fn touch(&self) {
        if let Ok(mut last) = self.last.lock() {
            *last = Instant::now();
        }
    }

    pub fn age(&self) -> Duration {
        self.last
            .lock()
            .map(|last| last.elapsed())
            .unwrap_or_default()
    }

    pub fn available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }
}

struct Actor {
    handler: HotkeyHandler,
    sink: Box<dyn RecordingSink>,
    keys: Box<dyn KeyStateSource>,
    capture: ShortcutCapture,
    pending: Option<PendingTimer>,
    heartbeat: ListenerHeartbeat,
}

impl Actor {
    fn handle_event(&mut self, kind: RawEventType, vk: i64, flags: u64) {
        if kind == RawEventType::KeyDown && vk == ESCAPE_KEYCODE && self.sink.is_recording() {
            self.sink.cancel();
            return;
        }
        if self
            .handler
            .spec()
            .is_some_and(|spec| spec.vk() == vk)
            && self.capture.is_active(true)
        {
            let Actor {
                handler,
                sink,
                capture,
                ..
            } = self;
            let mut guarded = SinkRef { sink: &mut **sink, capture };
            handler.recover(&mut guarded);
            return;
        }

        let Actor {
            handler,
            sink,
            keys,
            capture,
            ..
        } = self;
        let mut guarded = SinkRef { sink: &mut **sink, capture };
        let (_, timer) = handler.handle(kind, vk, flags, &mut guarded, |code| keys.key_down(code));
        if let Some(request) = timer {
            self.pending = Some(PendingTimer {
                request,
                deadline: Instant::now() + request.delay,
            });
        }
    }

    fn recover(&mut self) {
        let Actor {
            handler,
            sink,
            capture,
            ..
        } = self;
        let mut guarded = SinkRef { sink: &mut **sink, capture };
        handler.recover(&mut guarded);
        self.pending = None;
    }

    fn handle_registered(&mut self, pressed: bool) {
        if self.capture.is_active(true) {
            if !pressed {
                self.handler.clear_pressed();
            }
            return;
        }
        let mask = self
            .handler
            .spec()
            .map(HotkeySpec::mask)
            .unwrap_or_default();
        let kind = if pressed {
            RawEventType::KeyDown
        } else {
            RawEventType::KeyUp
        };
        let vk = self
            .handler
            .spec()
            .map(HotkeySpec::vk)
            .unwrap_or_default();
        let Actor {
            handler,
            sink,
            capture,
            ..
        } = self;
        let mut guarded = SinkRef { sink: &mut **sink, capture };
        handler.handle(kind, vk, mask, &mut guarded, |_| false);
    }

    fn fire_due_timer(&mut self) {
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        if Instant::now() < pending.deadline {
            return;
        }
        let generation = pending.request.generation;
        self.pending = None;
        let Actor {
            handler,
            sink,
            capture,
            ..
        } = self;
        let mut guarded = SinkRef { sink: &mut **sink, capture };
        handler.on_timer(generation, &mut guarded);
    }

    fn reconcile_release(&mut self) {
        if !self.handler.pressed() {
            return;
        }
        let Some(spec) = self.handler.spec().cloned() else {
            return;
        };
        let key_down = self.keys.key_down(spec.vk());
        let flags = self.keys.flags();
        let Actor { handler, sink, .. } = self;
        handler.reconcile_release(&mut **sink, key_down, flags);
    }

    fn wait(&self, now: Instant) -> Duration {
        let mut wait = RECONCILE_INTERVAL;
        if let Some(pending) = self.pending.as_ref() {
            wait = wait.min(pending.deadline.saturating_duration_since(now));
        }
        wait
    }

    fn run(mut self, receiver: Receiver<TapMessage>) {
        let mut next_reconcile = Instant::now() + RECONCILE_INTERVAL;
        loop {
            self.heartbeat.touch();
            let now = Instant::now();
            match receiver.recv_timeout(self.wait(now)) {
                Ok(TapMessage::Event { kind, vk, flags }) => self.handle_event(kind, vk, flags),
                Ok(TapMessage::Registered { pressed }) => self.handle_registered(pressed),
                Ok(TapMessage::Recovered) => {
                    self.recover();
                }
                Ok(TapMessage::Stop) => break,
                Err(RecvTimeoutError::Timeout) => {
                    let now = Instant::now();
                    self.fire_due_timer();
                    if now >= next_reconcile {
                        self.reconcile_release();
                        next_reconcile = now + RECONCILE_INTERVAL;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }
}

pub struct ListenerHandle {
    control: Option<Sender<TapMessage>>,
    tap: Option<TapHandle>,
    carbon: Option<CarbonHotkey>,
    monitor: Option<GlobalMonitor>,
    actor: Option<JoinHandle<()>>,
    heartbeat: ListenerHeartbeat,
    available: Arc<AtomicBool>,
}

impl ListenerHandle {
    /// Install a listen-only CGEventTap and drive the gesture actor.
    pub fn spawn_passive(
        spec: HotkeySpec,
        mode: HotkeyMode,
        capture_file: PathBuf,
        sink: Box<dyn RecordingSink>,
        keys: Box<dyn KeyStateSource>,
    ) -> Result<Self> {
        let (sender, receiver) = bounded(256);
        let tap = tap::start(sender.clone())?;
        let tap_available = tap.availability();
        let mut handle = Self::spawn_with_channel(
            spec,
            mode,
            capture_file,
            sink,
            keys,
            receiver,
            sender,
        )?;
        handle.tap = Some(tap);
        handle.heartbeat.available = tap_available.clone();
        handle.available = tap_available;
        Ok(handle)
    }

    /// Drive the actor from an externally installed source (AppKit monitor).
    /// `control` must feed the same channel as `receiver`; it is used to stop
    /// the actor even while other senders are still alive.
    pub fn spawn_with_channel(
        spec: HotkeySpec,
        mode: HotkeyMode,
        capture_file: PathBuf,
        sink: Box<dyn RecordingSink>,
        keys: Box<dyn KeyStateSource>,
        receiver: Receiver<TapMessage>,
        control: Sender<TapMessage>,
    ) -> Result<Self> {
        let available = Arc::new(AtomicBool::new(true));
        let heartbeat = ListenerHeartbeat::new(available.clone());
        let actor = Actor {
            handler: HotkeyHandler::new(Some(spec), mode),
            sink,
            keys,
            capture: ShortcutCapture::new(capture_file),
            pending: None,
            heartbeat: heartbeat.clone(),
        };
        let thread = thread::Builder::new()
            .name("veyqa-hotkey-logic".to_string())
            .spawn(move || actor.run(receiver))?;
        Ok(Self {
            control: Some(control),
            tap: None,
            carbon: None,
            monitor: None,
            actor: Some(thread),
            heartbeat,
            available,
        })
    }

    /// Register one exact Carbon chord. No event tap or permission is needed.
    pub fn spawn_registered(
        spec: HotkeySpec,
        mode: HotkeyMode,
        capture_file: PathBuf,
        sink: Box<dyn RecordingSink>,
    ) -> Result<Self> {
        let (sender, receiver) = bounded(256);
        let carbon = CarbonHotkey::start(&spec, sender.clone())
            .map_err(|error| anyhow::anyhow!(error))?;
        let control = sender.clone();
        let mut handle = Self::spawn_with_channel(
            spec,
            mode,
            capture_file,
            sink,
            Box::new(SystemKeyState),
            receiver,
            control,
        )?;
        handle.available.store(carbon.is_registered(), Ordering::SeqCst);
        handle.heartbeat.available = handle.available.clone();
        handle.carbon = Some(carbon);
        Ok(handle)
    }

    /// AppKit global monitor for managed Macs that reject event taps.
    pub fn spawn_monitor(
        spec: HotkeySpec,
        mode: HotkeyMode,
        capture_file: PathBuf,
        sink: Box<dyn RecordingSink>,
    ) -> Result<Self> {
        let (sender, receiver) = bounded(256);
        let monitor = GlobalMonitor::install(sender.clone())
            .map_err(|error| anyhow::anyhow!(error))?;
        let control = sender.clone();
        let mut handle = Self::spawn_with_channel(
            spec,
            mode,
            capture_file,
            sink,
            Box::new(SystemKeyState),
            receiver,
            control,
        )?;
        handle.monitor = Some(monitor);
        Ok(handle)
    }

    pub fn heartbeat(&self) -> ListenerHeartbeat {
        self.heartbeat.clone()
    }

    pub fn stop(&mut self) {
        if let Some(control) = self.control.as_ref() {
            let _ = control.send_timeout(TapMessage::Stop, Duration::from_millis(100));
        }
        if let Some(mut tap) = self.tap.take() {
            tap.stop();
        }
        if let Some(mut monitor) = self.monitor.take() {
            monitor.remove();
        }
        self.carbon.take();
        self.control.take();
        if let Some(actor) = self.actor.take() {
            let _ = actor.join();
        }
    }
}

impl Drop for ListenerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::HotkeySpec;
    use crate::hotkey::Modifier;

    #[derive(Debug, Default)]
    struct FakeState {
        recording: bool,
        actions: Vec<&'static str>,
    }

    #[derive(Clone, Default)]
    struct FakeSink {
        state: Arc<std::sync::Mutex<FakeState>>,
    }

    impl FakeSink {
        fn new(state: &Arc<std::sync::Mutex<FakeState>>) -> Self {
            Self {
                state: state.clone(),
            }
        }
    }

    impl RecordingSink for FakeSink {
        fn start(&mut self) -> bool {
            let mut state = self.state.lock().unwrap();
            state.actions.push("start");
            state.recording = true;
            true
        }

        fn stop(&mut self) {
            let mut state = self.state.lock().unwrap();
            state.actions.push("stop");
            state.recording = false;
        }

        fn cancel(&mut self) {
            let mut state = self.state.lock().unwrap();
            state.actions.push("cancel");
            state.recording = false;
        }

        fn toggle(&mut self) {
            let mut state = self.state.lock().unwrap();
            state.actions.push("toggle");
            state.recording = !state.recording;
        }

        fn is_recording(&self) -> bool {
            self.state.lock().unwrap().recording
        }
    }

    fn right_option() -> HotkeySpec {
        HotkeySpec::Modifier {
            vk: 61,
            modifier: Modifier::Option,
        }
    }

    struct Harness {
        handle: ListenerHandle,
        sender: Sender<TapMessage>,
        state: Arc<std::sync::Mutex<FakeState>>,
    }

    impl Harness {
        fn send_event(&self, kind: RawEventType, vk: i64, flags: u64) {
            self.sender
                .send(TapMessage::Event { kind, vk, flags })
                .unwrap();
        }

        fn actions(&self) -> Vec<&'static str> {
            self.state.lock().unwrap().actions.clone()
        }

        fn has_action(&self, action: &str) -> bool {
            self.state
                .lock()
                .unwrap()
                .actions
                .iter()
                .any(|item| *item == action)
        }

        fn set_recording(&self, recording: bool) {
            self.state.lock().unwrap().recording = recording;
        }

        fn clear_actions(&self) {
            self.state.lock().unwrap().actions.clear();
        }
    }

    fn start_actor(
        spec: HotkeySpec,
        mode: HotkeyMode,
        capture: PathBuf,
        keys: Box<dyn KeyStateSource>,
        recording: bool,
    ) -> Harness {
        let state = Arc::new(std::sync::Mutex::new(FakeState {
            recording,
            actions: Vec::new(),
        }));
        let (sender, receiver) = bounded(64);
        let control = sender.clone();
        let handle = ListenerHandle::spawn_with_channel(
            spec,
            mode,
            capture,
            Box::new(FakeSink::new(&state)),
            keys,
            receiver,
            control,
        )
        .unwrap();
        Harness {
            handle,
            sender,
            state,
        }
    }

    fn wait_for<F: FnMut() -> bool>(mut predicate: F, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            if predicate() {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }

    fn temp_capture_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "veyqa-hotkey-{name}-{}-{:?}.lock",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    struct StaticKeys {
        down: bool,
        flags: u64,
    }

    impl KeyStateSource for StaticKeys {
        fn key_down(&mut self, _vk: i64) -> bool {
            self.down
        }

        fn flags(&mut self) -> u64 {
            self.flags
        }
    }

    #[test]
    fn hold_mode_starts_after_the_right_option_guard_and_stops_on_release() {
        let capture = temp_capture_path("hold");
        let mut harness = start_actor(
            right_option(),
            HotkeyMode::Hold,
            capture,
            Box::new(StaticKeys {
                down: false,
                flags: 0,
            }),
            false,
        );
        harness.send_event(RawEventType::FlagsChanged, 61, 0x0008_0000 | 0x40);
        wait_for(|| harness.has_action("start"), "start");
        harness.send_event(RawEventType::FlagsChanged, 61, 0);
        wait_for(|| harness.has_action("stop"), "stop");
        assert_eq!(harness.actions(), vec!["start", "stop"]);
        harness.handle.stop();
    }

    #[test]
    fn escape_cancels_only_while_recording() {
        let capture = temp_capture_path("escape");
        let mut harness = start_actor(
            right_option(),
            HotkeyMode::Toggle,
            capture,
            Box::new(StaticKeys {
                down: false,
                flags: 0,
            }),
            true,
        );
        harness.send_event(RawEventType::KeyDown, ESCAPE_KEYCODE, 0);
        wait_for(|| harness.has_action("cancel"), "cancel");
        harness.set_recording(false);
        harness.clear_actions();
        harness.send_event(RawEventType::KeyDown, ESCAPE_KEYCODE, 0);
        thread::sleep(Duration::from_millis(80));
        assert!(harness.actions().is_empty());
        harness.handle.stop();
    }

    #[test]
    fn shortcut_capture_file_suppresses_the_gesture() {
        let capture = temp_capture_path("suppress");
        fs::write(&capture, std::process::id().to_string()).unwrap();
        let mut harness = start_actor(
            right_option(),
            HotkeyMode::Hold,
            capture.clone(),
            Box::new(StaticKeys {
                down: false,
                flags: 0,
            }),
            false,
        );
        harness.send_event(RawEventType::FlagsChanged, 61, 0x0008_0000 | 0x40);
        thread::sleep(Duration::from_millis(300));
        assert!(harness.actions().is_empty());
        harness.handle.stop();
        let _ = fs::remove_file(&capture);
    }

    #[test]
    fn dead_capture_pid_is_removed_and_does_not_suppress() {
        let capture = temp_capture_path("dead");
        fs::write(&capture, "999999").unwrap();
        let mut guard = ShortcutCapture::new(capture.clone());
        assert!(!guard.is_active(true));
        assert!(!capture.exists());
    }

    #[test]
    fn lost_release_is_confirmed_by_two_polls() {
        let capture = temp_capture_path("lost");
        let mut harness = start_actor(
            right_option(),
            HotkeyMode::Hold,
            capture,
            Box::new(StaticKeys {
                down: false,
                flags: 0,
            }),
            false,
        );
        harness.send_event(RawEventType::FlagsChanged, 61, 0x0008_0000 | 0x40);
        wait_for(|| harness.has_action("start"), "start");
        wait_for(|| harness.has_action("stop"), "stop from reconcile polls");
        harness.handle.stop();
    }
}
