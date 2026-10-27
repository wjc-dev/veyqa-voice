//! Listen-only CGEventTap with a deterministic repair loop.
//!
//! Mirrors `_PassiveTapListener` in `src/keyboard/listener.py`: the callback
//! never decides anything, it only forwards the raw event, and a one-second
//! run-loop slice re-enables a tap that macOS disabled.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{bail, Result};
use crossbeam_channel::{bounded, Sender};
use objc2_core_foundation::{
    kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFMachPort, CFRetained, CFRunLoop,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventMask, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType,
};

use super::RawEventType;

/// Raw events and lifecycle notices from the tap thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TapMessage {
    Event {
        kind: RawEventType,
        vk: i64,
        flags: u64,
    },
    /// Carbon delivered the registered chord's press/release pair.
    Registered { pressed: bool },
    /// macOS disabled the tap and the repair loop re-enabled it.
    Recovered,
    /// Internal shutdown signal for the gesture actor.
    Stop,
}

struct TapState {
    sender: Sender<TapMessage>,
    /// The live tap, so the callback can re-enable it immediately like
    /// `CGEventTapEnable` in the Python listener.
    tap: AtomicPtr<CFMachPort>,
}

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    refcon: *mut c_void,
) -> *mut CGEvent {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let state = unsafe { &*refcon.cast::<TapState>() };
        let raw_type = event_type.0;
        if raw_type == 0xFFFF_FFFE || raw_type == 0xFFFF_FFFF {
            let tap = state.tap.load(Ordering::SeqCst);
            if !tap.is_null() {
                CGEvent::tap_enable(unsafe { &*tap }, true);
            }
            let _ = state.sender.try_send(TapMessage::Recovered);
            return;
        }
        let kind = match raw_type {
            10 => RawEventType::KeyDown,
            11 => RawEventType::KeyUp,
            12 => RawEventType::FlagsChanged,
            _ => return,
        };
        let event_ref = unsafe { event.as_ref() };
        let vk = CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode);
        let flags = CGEvent::flags(Some(event_ref)).0;
        let _ = state.sender.try_send(TapMessage::Event { kind, vk, flags });
    }));
    event.as_ptr()
}

pub struct TapHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    available: Arc<AtomicBool>,
}

impl TapHandle {
    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }

    pub fn availability(&self) -> Arc<AtomicBool> {
        self.available.clone()
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for TapHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Install the listen-only tap on a dedicated run-loop thread.
///
/// The returned handle is only created after `CGEventTapCreate` succeeded, so
/// a missing Input Monitoring grant surfaces as an error the app can report.
pub fn start(sender: Sender<TapMessage>) -> Result<TapHandle> {
    let (ready_tx, ready_rx) = bounded(1);
    let stop = Arc::new(AtomicBool::new(false));
    let available = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread_available = available.clone();
    let thread = thread::Builder::new()
        .name("veyqa-hotkey-tap".to_string())
        .spawn(move || run(sender, ready_tx, thread_stop, thread_available))
        .map_err(|error| anyhow::anyhow!("无法启动快捷键线程: {error}"))?;

    match ready_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => Ok(TapHandle {
            stop,
            thread: Some(thread),
            available,
        }),
        Ok(Err(error)) => {
            let _ = thread.join();
            bail!(error)
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            let _ = thread.join();
            bail!("快捷键监听启动超时")
        }
    }
}

fn run(
    sender: Sender<TapMessage>,
    ready: Sender<std::result::Result<(), String>>,
    stop: Arc<AtomicBool>,
    available: Arc<AtomicBool>,
) {
    let state = Box::into_raw(Box::new(TapState {
        sender: sender.clone(),
        tap: AtomicPtr::new(std::ptr::null_mut()),
    }));
    let mask = ((1u64 << CGEventType::FlagsChanged.0)
        | (1u64 << CGEventType::KeyDown.0)
        | (1u64 << CGEventType::KeyUp.0)) as CGEventMask;

    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            mask,
            Some(callback),
            state.cast::<c_void>(),
        )
    };
    let Some(tap) = tap else {
        // The tap was never installed, so the callback cannot run.
        unsafe { drop(Box::from_raw(state)) };
        let _ = ready.send(Err("CGEventTapCreate 失败：请检查“输入监控”授权".to_string()));
        return;
    };
    unsafe { &*state }
        .tap
        .store(CFRetained::as_ptr(&tap).as_ptr(), Ordering::SeqCst);

    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&tap), 0) else {
        tap.invalidate();
        unsafe { drop(Box::from_raw(state)) };
        let _ = ready.send(Err("无法创建快捷键 run loop source".to_string()));
        return;
    };
    let Some(run_loop) = CFRunLoop::current() else {
        tap.invalidate();
        unsafe { drop(Box::from_raw(state)) };
        let _ = ready.send(Err("无法获取快捷键 CFRunLoop".to_string()));
        return;
    };
    let common_modes = unsafe { kCFRunLoopCommonModes };
    run_loop.add_source(Some(&source), common_modes);
    CGEvent::tap_enable(&tap, true);
    available.store(true, Ordering::SeqCst);
    let _ = ready.send(Ok(()));

    while !stop.load(Ordering::SeqCst) {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 1.0, false);
        if CGEvent::tap_is_enabled(&tap) {
            continue;
        }
        CGEvent::tap_enable(&tap, true);
        if CGEvent::tap_is_enabled(&tap) {
            let _ = sender.try_send(TapMessage::Recovered);
        }
    }

    available.store(false, Ordering::SeqCst);
    unsafe { &*state }.tap.store(std::ptr::null_mut(), Ordering::SeqCst);
    run_loop.remove_source(Some(&source), common_modes);
    tap.invalidate();
    unsafe { drop(Box::from_raw(state)) };
}
