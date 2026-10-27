//! Native `NSStatusItem` menu bar, the optional Dock entry, and the app menu.
//!
//! Ported from `src/ui/status_bar.py`. Only the main thread may touch these
//! objects, which `MainThreadMarker` enforces at compile time.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSCellImagePosition, NSEventMask, NSImage,
    NSImageScaling, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSSquareStatusItemLength,
};
use objc2_foundation::{ns_string, NSString, NSSize};

use crate::paths;
use crate::state::{self, InputState};

pub struct MenuCallbacks {
    pub open_settings: Box<dyn FnMut()>,
    pub check_updates: Box<dyn FnMut()>,
}

struct ActionHandlerIvars {
    callbacks: Rc<RefCell<MenuCallbacks>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ActionHandlerIvars]
    struct ActionHandler;

    unsafe impl NSObjectProtocol for ActionHandler {}

    impl ActionHandler {
        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            (self.ivars().callbacks.borrow_mut().open_settings)();
        }

        #[unsafe(method(handleStatusItem:))]
        fn handle_status_item(&self, _sender: Option<&AnyObject>) {
            (self.ivars().callbacks.borrow_mut().open_settings)();
        }

        #[unsafe(method(checkUpdates:))]
        fn check_updates(&self, _sender: Option<&AnyObject>) {
            (self.ivars().callbacks.borrow_mut().check_updates)();
        }
    }
);

impl ActionHandler {
    fn new(mtm: MainThreadMarker, callbacks: Rc<RefCell<MenuCallbacks>>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ActionHandlerIvars { callbacks });
        unsafe { msg_send![super(this), init] }
    }
}

pub struct MenuBar {
    status_item: Option<Retained<NSStatusItem>>,
    updates_item: Option<Retained<NSMenuItem>>,
    handler: Retained<ActionHandler>,
    custom_icon: Option<Retained<NSImage>>,
}

impl MenuBar {
    pub fn new(mtm: MainThreadMarker, callbacks: Rc<RefCell<MenuCallbacks>>) -> Self {
        let handler = ActionHandler::new(mtm, callbacks);
        let custom_icon = load_custom_icon(mtm);
        let mut menu = Self {
            status_item: None,
            updates_item: None,
            handler,
            custom_icon,
        };
        if state::menu_bar_enabled() {
            menu.install_status_item(mtm);
        }
        menu
    }

    /// Install the application menu bar (visible while the Dock icon is shown).
    pub fn install_app_menu(&mut self, mtm: MainThreadMarker, app: &NSApplication) {
        let bar = NSMenu::new(mtm);
        let top = NSMenuItem::new(mtm);
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Veyqa Voice"));

        let settings = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("设置…"),
                Some(sel!(openSettings:)),
                ns_string!(","),
            )
        };
        unsafe { settings.setTarget(Some(handler_object(&self.handler))) };
        menu.addItem(&settings);

        let updates = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("检查更新…"),
                Some(sel!(checkUpdates:)),
                ns_string!(""),
            )
        };
        // Enabled once the native Sparkle controller loads.
        updates.setEnabled(false);
        unsafe { updates.setTarget(Some(handler_object(&self.handler))) };
        menu.addItem(&updates);
        self.updates_item = Some(updates);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let quit = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("退出 Veyqa Voice"),
                Some(sel!(terminate:)),
                ns_string!("q"),
            )
        };
        menu.addItem(&quit);

        top.setSubmenu(Some(&menu));
        bar.addItem(&top);
        app.setMainMenu(Some(&bar));
    }

    /// Mirror `apply_preferences`: create or drop the status item without a restart.
    pub fn apply_menu_bar_visible(&mut self, mtm: MainThreadMarker, enabled: bool) {
        if enabled {
            if self.status_item.is_none() {
                self.install_status_item(mtm);
            }
        } else if let Some(item) = self.status_item.take() {
            NSStatusBar::systemStatusBar().removeStatusItem(&item);
        }
    }

    pub fn set_check_updates_enabled(&self, enabled: bool) {
        if let Some(item) = self.updates_item.as_ref() {
            item.setEnabled(enabled);
        }
    }

    pub fn status_item_ready(&self, mtm: MainThreadMarker) -> bool {
        self.status_item
            .as_ref()
            .and_then(|item| item.button(mtm))
            .is_some()
    }

    pub fn apply_state(&self, mtm: MainThreadMarker, state: InputState, queue_length: usize) {
        let Some(item) = self.status_item.as_ref() else {
            return;
        };
        let Some(button) = item.button(mtm) else {
            return;
        };
        let display = state::status_display(state, queue_length, self.custom_icon.is_some());
        if let Some(icon) = self.custom_icon.as_ref() {
            icon.setSize(NSSize::new(18.0, 18.0));
            button.setImage(Some(icon));
            button.setTitle(&NSString::from_str(display.title));
            button.setImageScaling(NSImageScaling::ScaleProportionallyDown);
            button.setImagePosition(NSCellImagePosition::ImageOnly);
        } else {
            button.setImage(None);
            button.setTitle(&NSString::from_str(display.title));
            button.setImagePosition(NSCellImagePosition::NoImage);
        }
        button.setToolTip(Some(&NSString::from_str(&display.tooltip)));
    }

    /// 30 s status-item health check: recreate a lost button and restore a
    /// hidden icon, mirroring `_schedule_status_health_check`.
    pub fn health_check(&mut self, mtm: MainThreadMarker, enabled: bool) {
        if !enabled {
            return;
        }
        let needs_install = match self.status_item.as_ref() {
            None => true,
            Some(item) => item.button(mtm).is_none(),
        };
        if needs_install {
            self.install_status_item(mtm);
            return;
        }
        if let Some(item) = self.status_item.as_ref() {
            if !item.isVisible() {
                item.setVisible(true);
            }
        }
    }

    fn install_status_item(&mut self, mtm: MainThreadMarker) {
        let bar = NSStatusBar::systemStatusBar();
        if let Some(old) = self.status_item.take() {
            bar.removeStatusItem(&old);
        }
        let item = bar.statusItemWithLength(NSSquareStatusItemLength);
        item.setVisible(true);
        let Some(button) = item.button(mtm) else {
            bar.removeStatusItem(&item);
            return;
        };
        button.setTitle(ns_string!("◉"));
        button.setToolTip(Some(ns_string!("Veyqa Voice - 点击打开设置")));
        button.setEnabled(true);
        unsafe {
            button.setTarget(Some(handler_object(&self.handler)));
            button.setAction(Some(sel!(handleStatusItem:)));
        }
        button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
        self.status_item = Some(item);
        self.apply_state(mtm, InputState::Idle, 0);
    }
}

fn handler_object(handler: &Retained<ActionHandler>) -> &AnyObject {
    unsafe { &*(handler.as_ref() as *const ActionHandler as *const AnyObject) }
}

fn load_custom_icon(_mtm: MainThreadMarker) -> Option<Retained<NSImage>> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(configured) = std::env::var_os("STATUS_ICON_PNG") {
        candidates.push(PathBuf::from(configured));
    }
    candidates.push(paths::resource_root().join("StatusBarIcon.png"));
    candidates.push(paths::resource_root().join("packaging/StatusBarIcon.png"));
    let path = candidates.into_iter().find(|path| path.is_file())?;
    let image =
        NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(&path.to_string_lossy()))?;
    image.setTemplate(true);
    Some(image)
}

/// `menu_bar_enabled()` can make the Dock entry mandatory when no status item
/// could be created (macOS hides the icon while the menu bar is full).
pub fn activation_policy(mtm: MainThreadMarker, menu_bar: &MenuBar) -> NSApplicationActivationPolicy {
    if state::dock_icon_enabled() || !menu_bar.status_item_ready(mtm) {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_visibility_follows_the_same_env_flags_as_python() {
        // These read the process environment; assert the pure composition.
        let dock = std::env::var("SHOW_DOCK_ICON").unwrap_or_else(|_| "true".into());
        let menu = std::env::var("SHOW_MENU_BAR_ICON").unwrap_or_else(|_| "false".into());
        let expected = dock.to_lowercase() != "true" || menu.to_lowercase() == "true";
        assert_eq!(state::menu_bar_enabled(), expected);
    }
}
