//! AppKit global-monitor fallback for machines that block event taps.
//!
//! Must be installed and removed on the main thread, like the Python app's
//! `AppHelper.callAfter(install_monitor)` path.

use std::ptr::NonNull;

use block2::RcBlock;
use crossbeam_channel::Sender;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventType};

use super::tap::TapMessage;
use super::RawEventType;

pub struct GlobalMonitor {
    observer: Option<Retained<AnyObject>>,
}

impl GlobalMonitor {
    pub fn install(sender: Sender<TapMessage>) -> Result<Self, String> {
        let mask = NSEventMask::FlagsChanged | NSEventMask::KeyDown | NSEventMask::KeyUp;
        let block = RcBlock::new(move |event: NonNull<NSEvent>| {
            let event = unsafe { event.as_ref() };
            let raw_type = event.r#type().0;
            let kind = if raw_type == NSEventType::KeyDown.0 {
                RawEventType::KeyDown
            } else if raw_type == NSEventType::KeyUp.0 {
                RawEventType::KeyUp
            } else if raw_type == NSEventType::FlagsChanged.0 {
                RawEventType::FlagsChanged
            } else {
                return;
            };
            let vk = event.keyCode() as i64;
            let flags = event.modifierFlags().0 as u64;
            let _ = sender.try_send(TapMessage::Event { kind, vk, flags });
        });
        let observer = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &block)
            .ok_or_else(|| "macOS 未允许只读快捷键监听".to_string())?;
        Ok(Self {
            observer: Some(observer),
        })
    }

    pub fn remove(&mut self) {
        if let Some(observer) = self.observer.take() {
            unsafe { NSEvent::removeMonitor(&observer) };
        }
    }
}

impl Drop for GlobalMonitor {
    fn drop(&mut self) {
        self.remove();
    }
}
