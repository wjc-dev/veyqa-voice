//! Carbon `RegisterEventHotKey` backend for chord shortcuts.
//!
//! Mirrors `_RegisteredHotKeyListener` in Python: only the exact registered
//! chord is delivered, so this path needs no Input Monitoring permission and
//! never observes other keystrokes. The callback only enqueues an event.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use crossbeam_channel::Sender;

use super::tap::TapMessage;
use super::{HotkeySpec, Modifier};

const EVENT_CLASS_KEYBOARD: u32 = 0x6b65_7962; // 'keyb'
const EVENT_HOTKEY_PRESSED: u32 = 5;
const EVENT_HOTKEY_RELEASED: u32 = 6;

const CARBON_COMMAND: u32 = 1 << 8;
const CARBON_SHIFT: u32 = 1 << 9;
const CARBON_OPTION: u32 = 1 << 11;
const CARBON_CONTROL: u32 = 1 << 12;
const CARBON_FUNCTION: u32 = 1 << 23;

type EventRef = *mut libc::c_void;
type EventTargetRef = *mut libc::c_void;
type EventHandlerRef = *mut libc::c_void;
type EventHotKeyRef = *mut libc::c_void;
type OSStatus = i32;

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
struct EventHotKeyId {
    signature: u32,
    identifier: u32,
}

type EventHandler = extern "C" fn(*mut libc::c_void, EventRef, *mut libc::c_void) -> OSStatus;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn GetEventKind(event: EventRef) -> u32;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandler,
        count: usize,
        types: *const EventTypeSpec,
        user_data: *mut libc::c_void,
        out: *mut EventHandlerRef,
    ) -> OSStatus;
    fn RemoveEventHandler(handler: EventHandlerRef) -> OSStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        identifier: EventHotKeyId,
        target: EventTargetRef,
        options: u32,
        out: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hotkey: EventHotKeyRef) -> OSStatus;
}

struct CallbackContext {
    sender: Sender<TapMessage>,
}

extern "C" fn on_event(
    _call_ref: *mut libc::c_void,
    event: EventRef,
    user_data: *mut libc::c_void,
) -> OSStatus {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if user_data.is_null() || event.is_null() {
            return;
        }
        let kind = unsafe { GetEventKind(event) };
        let pressed = match kind {
            EVENT_HOTKEY_PRESSED => true,
            EVENT_HOTKEY_RELEASED => false,
            _ => return,
        };
        let context = unsafe { &*(user_data as *const CallbackContext) };
        let _ = context.sender.try_send(TapMessage::Registered { pressed });
    }));
    0
}

pub struct CarbonHotkey {
    handler: EventHandlerRef,
    hotkey: EventHotKeyRef,
    context: *mut CallbackContext,
}

unsafe impl Send for CarbonHotkey {}

impl CarbonHotkey {
    pub fn start(spec: &HotkeySpec, sender: Sender<TapMessage>) -> Result<Self, String> {
        let HotkeySpec::Key { vk, modifiers } = spec else {
            return Err("系统注册型快捷键必须包含普通按键和至少一个修饰键".to_string());
        };
        if modifiers.is_empty() {
            return Err("系统注册型快捷键必须包含普通按键和至少一个修饰键".to_string());
        }

        let context = Box::into_raw(Box::new(CallbackContext { sender }));
        let types = [
            EventTypeSpec {
                event_class: EVENT_CLASS_KEYBOARD,
                event_kind: EVENT_HOTKEY_PRESSED,
            },
            EventTypeSpec {
                event_class: EVENT_CLASS_KEYBOARD,
                event_kind: EVENT_HOTKEY_RELEASED,
            },
        ];
        let target = unsafe { GetApplicationEventTarget() };
        let mut handler: EventHandlerRef = ptr::null_mut();
        let status = unsafe {
            InstallEventHandler(
                target,
                on_event,
                types.len(),
                types.as_ptr(),
                context.cast(),
                &mut handler,
            )
        };
        if status != 0 {
            unsafe { drop(Box::from_raw(context)) };
            return Err(format!("无法安装系统快捷键处理器（{status}）"));
        }

        let identifier = EventHotKeyId {
            signature: u32::from_be_bytes(*b"VInp"),
            identifier: 1,
        };
        let mut hotkey: EventHotKeyRef = ptr::null_mut();
        let status = unsafe {
            RegisterEventHotKey(
                *vk as u32,
                carbon_modifiers(modifiers),
                identifier,
                target,
                0,
                &mut hotkey,
            )
        };
        if status != 0 {
            unsafe {
                RemoveEventHandler(handler);
                drop(Box::from_raw(context));
            }
            return Err(format!("快捷键已被其他应用占用或注册失败（{status}）"));
        }

        Ok(Self {
            handler,
            hotkey,
            context,
        })
    }

    pub fn is_registered(&self) -> bool {
        !self.hotkey.is_null()
    }
}

impl Drop for CarbonHotkey {
    fn drop(&mut self) {
        unsafe {
            if !self.hotkey.is_null() {
                UnregisterEventHotKey(self.hotkey);
            }
            if !self.handler.is_null() {
                RemoveEventHandler(self.handler);
            }
            if !self.context.is_null() {
                drop(Box::from_raw(self.context));
            }
        }
    }
}

fn carbon_modifiers(modifiers: &[Modifier]) -> u32 {
    modifiers
        .iter()
        .fold(0, |mask, modifier| {
            mask | match modifier {
                Modifier::Command => CARBON_COMMAND,
                Modifier::Shift => CARBON_SHIFT,
                Modifier::Option => CARBON_OPTION,
                Modifier::Control => CARBON_CONTROL,
                Modifier::Function => CARBON_FUNCTION,
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_bits_match_carbon() {
        assert_eq!(carbon_modifiers(&[Modifier::Command]), 1 << 8);
        assert_eq!(
            carbon_modifiers(&[Modifier::Control, Modifier::Option]),
            (1 << 12) | (1 << 11)
        );
        assert_eq!(carbon_modifiers(&[Modifier::Function]), 1 << 23);
        assert_eq!(carbon_modifiers(&[]), 0);
    }

    #[test]
    fn only_chords_are_registerable() {
        let (sender, _receiver) = crossbeam_channel::bounded(1);
        let modifier = HotkeySpec::Modifier {
            vk: 61,
            modifier: Modifier::Option,
        };
        assert!(CarbonHotkey::start(&modifier, sender.clone()).is_err());
        let bare = HotkeySpec::Key {
            vk: 49,
            modifiers: Vec::new(),
        };
        assert!(CarbonHotkey::start(&bare, sender).is_err());
    }
}
