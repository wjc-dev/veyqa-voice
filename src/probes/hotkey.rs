use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy, CGEventType,
};

const RIGHT_OPTION: i64 = 61;
const LEFT_OPTION: i64 = 58;
const RIGHT_COMMAND: i64 = 54;
const ESCAPE: i64 = 53;

struct ProbeState {
    started: Instant,
    events: AtomicU64,
    disabled: AtomicU64,
}

fn key_name(keycode: i64) -> &'static str {
    match keycode {
        61 => "右Option",
        58 => "左Option",
        54 => "右Command",
        55 => "左Command",
        56 => "左Shift",
        60 => "右Shift",
        59 => "左Control",
        62 => "右Control",
        53 => "Esc",
        36 => "Return",
        49 => "Space",
        48 => "Tab",
        _ => "键",
    }
}

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    refcon: *mut c_void,
) -> *mut CGEvent {
    let state = unsafe { &*refcon.cast::<ProbeState>() };
    let micros = state.started.elapsed().as_micros();
    state.events.fetch_add(1, Ordering::Relaxed);

    let kind = event_type.0;
    if kind == 0xFFFF_FFFE || kind == 0xFFFF_FFFF {
        state.disabled.fetch_add(1, Ordering::Relaxed);
        println!("[{micros:>9}µs] tap 被系统禁用 (type=0x{kind:X})，等待修复循环");
        return event.as_ptr();
    }

    let event_ref = unsafe { event.as_ref() };
    let keycode = CGEvent::integer_value_field(Some(event_ref), objc2_core_graphics::CGEventField::KeyboardEventKeycode);
    let flags = CGEvent::flags(Some(event_ref)).0;
    let label = key_name(keycode);

    if kind == 10 {
        println!("[{micros:>9}µs] keyDown    {label:<10} code={keycode:<3} flags=0x{flags:X}");
    } else if kind == 11 {
        println!("[{micros:>9}µs] keyUp      {label:<10} code={keycode:<3} flags=0x{flags:X}");
    } else if kind == 12 {
        let down = (flags & 0x0008_0000) != 0;
        println!(
            "[{micros:>9}µs] flagsChanged {label:<10} code={keycode:<3} flags=0x{flags:X} alternate={down}"
        );
    } else {
        println!("[{micros:>9}µs] type={kind} code={keycode} flags=0x{flags:X}");
    }
    event.as_ptr()
}

pub fn run(seconds: f32, self_test: bool) -> Result<()> {
    let mask = ((1u64 << CGEventType::FlagsChanged.0)
        | (1u64 << CGEventType::KeyDown.0)
        | (1u64 << CGEventType::KeyUp.0)) as objc2_core_graphics::CGEventMask;

    let state = ProbeState {
        started: Instant::now(),
        events: AtomicU64::new(0),
        disabled: AtomicU64::new(0),
    };

    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            mask,
            Some(callback),
            (&state as *const ProbeState).cast_mut().cast::<c_void>(),
        )
    };
    let Some(tap) = tap else {
        bail!("CGEventTapCreate 失败：请检查“输入监控”授权");
    };

    let source = CFMachPort::new_run_loop_source(None, Some(&tap), 0)
        .ok_or_else(|| anyhow::anyhow!("无法创建 run loop source"))?;
    let run_loop = CFRunLoop::current().ok_or_else(|| anyhow::anyhow!("无法获取 CFRunLoop"))?;
    let mode = unsafe { kCFRunLoopDefaultMode };
    run_loop.add_source(Some(&source), mode);
    CGEvent::tap_enable(&tap, true);

    println!(
        "只读 tap 已启动（{}s）。按右 Option / 左右 Cmd / 组合键 / Esc，观察事件顺序。",
        seconds
    );
    if self_test {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(400));
            let Some(source) = CGEventSource::new(CGEventSourceStateID::CombinedSessionState) else {
                return;
            };
            let post = |keycode: u16, down: bool, kind: CGEventType, flags: CGEventFlags| {
                if let Some(event) = CGEvent::new_keyboard_event(Some(&source), keycode, down) {
                    CGEvent::set_type(Some(&event), kind);
                    CGEvent::set_flags(Some(&event), flags);
                    CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&event));
                }
            };
            post(LEFT_OPTION as u16, true, CGEventType::FlagsChanged, CGEventFlags::MaskAlternate);
            std::thread::sleep(Duration::from_millis(200));
            post(40, true, CGEventType::KeyDown, CGEventFlags::MaskAlternate);
            post(40, false, CGEventType::KeyUp, CGEventFlags::MaskAlternate);
            std::thread::sleep(Duration::from_millis(200));
            post(LEFT_OPTION as u16, false, CGEventType::FlagsChanged, CGEventFlags(0));
        });
    }
    let deadline = Instant::now() + Duration::from_secs_f32(seconds.max(0.5));
    let mut repairs = 0u32;
    while Instant::now() < deadline {
        CFRunLoop::run_in_mode(mode, 0.2, false);
        if !CGEvent::tap_is_enabled(&tap) {
            CGEvent::tap_enable(&tap, true);
            repairs += 1;
            println!("修复循环：重新启用 tap（第 {repairs} 次）");
        }
    }

    let pressed = CGEventSourceStateID::CombinedSessionState;
    println!();
    println!(
        "事件数={} tap被禁用={} 修复={} 状态源右Option按下={}",
        state.events.load(Ordering::Relaxed),
        state.disabled.load(Ordering::Relaxed),
        repairs,
        CGEventSource::key_state(pressed, RIGHT_OPTION as u16)
    );
    Ok(())
}
