//! AppKit application shell: menu bar, floating preview, and the 30 Hz pump.
//!
//! Every AppKit call runs on the main thread inside the `NSApplication` run
//! loop; the engine communicates with it only through `UiCommand`.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::Receiver;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSApplicationDelegate, NSApplicationTerminateReply, NSImage};
use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer, NSString};

use super::menu_bar::{self, MenuBar, MenuCallbacks};
use super::preview::PreviewWindow;
use super::{channel, ChannelUiSink, NoticeScope, UiCommand};
use crate::paths;
use crate::runtime::app::{install_stop_signals, stop_requested, AppRuntime};
use crate::runtime::hub::StateHub;
use crate::state::{self, InputState};
use crate::updater::{self, AppUpdater};

struct AppUi {
    mtm: MainThreadMarker,
    runtime: AppRuntime,
    receiver: Receiver<UiCommand>,
    menu: MenuBar,
    preview: PreviewWindow,
    updater: Option<AppUpdater>,
    last_health: Instant,
    last_status_health: Instant,
    startup_open_at: Option<Instant>,
}

impl AppUi {
    fn open_settings(&mut self) {
        let _ = self.runtime.open_settings();
    }

    fn check_updates(&mut self) {
        if let Some(updater) = self.updater.as_ref() {
            updater.check();
        }
    }

    fn pump(&mut self) {
        if stop_requested() {
            NSApplication::sharedApplication(self.mtm).terminate(None);
            return;
        }
        while let Ok(command) = self.receiver.try_recv() {
            self.apply_command(command);
        }
        self.preview.tick(self.mtm);

        let now = Instant::now();
        if now.duration_since(self.last_health) >= Duration::from_secs(2) {
            self.last_health = now;
            self.runtime.tick();
            if let Some(updater) = self.updater.as_mut() {
                updater.tick();
            }
            self.apply_preferences();
        }
        if now.duration_since(self.last_status_health) >= Duration::from_secs(30) {
            self.last_status_health = now;
            self.menu
                .health_check(self.mtm, state::menu_bar_enabled());
            self.apply_preferences();
        }
        if let Some(deadline) = self.startup_open_at {
            if now >= deadline {
                self.startup_open_at = None;
                if self.runtime.hub().state() == InputState::Idle {
                    self.open_settings();
                }
            }
        }
    }

    fn apply_command(&mut self, command: UiCommand) {
        match command {
            UiCommand::State(state) => self.menu.apply_state(self.mtm, state, 0),
            UiCommand::PreviewShow { preparing } => self.preview.show(self.mtm, preparing),
            UiCommand::PreviewHide => self.preview.hide(),
            UiCommand::PreviewText(text) => self.preview.set_text(&text),
            UiCommand::PreviewProcessing => self.preview.set_processing(),
            UiCommand::PreviewLevel(level) => self.preview.set_level(level),
            UiCommand::Notice { text, scope } => {
                if notice_relevant(&scope, &self.runtime.hub()) {
                    self.preview.show_notice(self.mtm, &text);
                }
            }
        }
    }

    /// Re-evaluate Dock/menu-bar entry points after a settings save.
    fn apply_preferences(&mut self) {
        self.menu
            .apply_menu_bar_visible(self.mtm, state::menu_bar_enabled());
        let policy = menu_bar::activation_policy(self.mtm, &self.menu);
        let app = NSApplication::sharedApplication(self.mtm);
        if app.activationPolicy() != policy {
            app.setActivationPolicy(policy);
        }
    }
}

struct DelegateIvars {
    ui: Weak<RefCell<AppUi>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateIvars]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _sender: &NSApplication, _visible: bool) -> bool {
            if let Some(ui) = self.ivars().ui.upgrade() {
                ui.borrow_mut().open_settings();
            }
            true
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _sender: &NSApplication) -> bool {
            false
        }

        /// Dock Quit / ⌘Q is intentional, unlike a supervisor signal.
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _sender: &NSApplication) -> NSApplicationTerminateReply {
            let _ = std::fs::write(paths::supervisor_paused_file(), "paused-by-user");
            NSApplicationTerminateReply::TerminateNow
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker, ui: Weak<RefCell<AppUi>>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars { ui });
        unsafe { msg_send![super(this), init] }
    }
}

struct PumpIvars {
    ui: Weak<RefCell<AppUi>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PumpIvars]
    struct PumpTarget;

    unsafe impl NSObjectProtocol for PumpTarget {}

    impl PumpTarget {
        #[unsafe(method(pump:))]
        fn pump(&self, _timer: &NSTimer) {
            if let Some(ui) = self.ivars().ui.upgrade() {
                ui.borrow_mut().pump();
            }
        }
    }
);

impl PumpTarget {
    fn new(mtm: MainThreadMarker, ui: Weak<RefCell<AppUi>>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PumpIvars { ui });
        unsafe { msg_send![super(this), init] }
    }
}

/// Resolve the signed outer bundle's icon explicitly; a bare executable would
/// otherwise inherit the generic app icon from LaunchServices.
fn install_application_icon(app: &NSApplication) {
    let Some(bundle) = paths::app_bundle_path() else {
        return;
    };
    let path = bundle.join("Contents/Resources/AppIcon.icns");
    if !path.is_file() {
        return;
    }
    let path = NSString::from_str(&path.to_string_lossy());
    let Some(icon) = NSImage::initWithContentsOfFile(NSImage::alloc(), &path) else {
        return;
    };
    unsafe { app.setApplicationIconImage(Some(&icon)) };
}

/// Run the resident menu-bar application.
pub fn run() -> Result<()> {
    let mtm = MainThreadMarker::new().context("界面必须在主线程启动")?;
    install_stop_signals();
    let (sender, receiver) = channel();
    let Some(runtime) = AppRuntime::start(Arc::new(ChannelUiSink::new(sender)))? else {
        return Ok(());
    };
    let startup_open_at = runtime
        .should_open_settings_on_start()
        .then(|| Instant::now() + Duration::from_millis(800));

    let callbacks = Rc::new(RefCell::new(MenuCallbacks {
        open_settings: Box::new(|| {}),
        check_updates: Box::new(|| {}),
    }));
    let mut menu = MenuBar::new(mtm, callbacks.clone());
    let app = NSApplication::sharedApplication(mtm);
    install_application_icon(&app);
    menu.install_app_menu(mtm, &app);

    let mut updater = None;
    if updater::available() {
        let data_dir = paths::data_dir();
        match paths::app_bundle_path() {
            Some(bundle) => match AppUpdater::load(mtm, &bundle, &data_dir, runtime.hub()) {
                Ok(loaded) => {
                    menu.set_check_updates_enabled(true);
                    updater = Some(loaded);
                }
                Err(_) => updater::unavailable(&data_dir),
            },
            None => updater::unavailable(&data_dir),
        }
    }

    let ui = Rc::new(RefCell::new(AppUi {
        mtm,
        runtime,
        receiver,
        menu,
        preview: PreviewWindow::new(),
        updater,
        last_health: Instant::now(),
        last_status_health: Instant::now(),
        startup_open_at,
    }));
    {
        let weak = Rc::downgrade(&ui);
        callbacks.borrow_mut().open_settings = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.borrow_mut().open_settings();
            }
        });
    }
    {
        let weak = Rc::downgrade(&ui);
        callbacks.borrow_mut().check_updates = Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.borrow_mut().check_updates();
            }
        });
    }

    let delegate = AppDelegate::new(mtm, Rc::downgrade(&ui));
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    ui.borrow_mut().apply_preferences();

    let pump = PumpTarget::new(mtm, Rc::downgrade(&ui));
    let timer = unsafe {
        NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
            1.0 / 30.0,
            &pump,
            sel!(pump:),
            None,
            true,
        )
    };
    timer.setTolerance(0.005);
    let run_loop = NSRunLoop::mainRunLoop();
    unsafe { run_loop.addTimer_forMode(&timer, NSRunLoopCommonModes) };

    crate::runtime::metrics::append_startup();
    app.run();
    ui.borrow_mut().runtime.shutdown();
    drop((delegate, pump, timer));
    Ok(())
}

/// Python's preview `is_relevant` predicates, evaluated on the main thread so a
/// notice queued before a new recording can never cover it.
fn notice_relevant(scope: &NoticeScope, hub: &StateHub) -> bool {
    if hub.is_recording() || hub.session_active() {
        return false;
    }
    match scope {
        NoticeScope::Idle => true,
        NoticeScope::FinishedSession(session_id) => {
            hub.finished_session_id().as_deref() == Some(session_id.as_str())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_are_dropped_once_a_newer_session_is_live() {
        let hub = StateHub::new(0.0);
        let idle = NoticeScope::Idle;
        let finished = NoticeScope::FinishedSession("a".into());
        assert!(notice_relevant(&idle, &hub));
        assert!(!notice_relevant(&finished, &hub));

        hub.note_finished_session("a".into());
        assert!(notice_relevant(&finished, &hub));
        assert!(!notice_relevant(
            &NoticeScope::FinishedSession("b".into()),
            &hub
        ));

        hub.set_session_active(true);
        assert!(!notice_relevant(&idle, &hub));
        assert!(!notice_relevant(&finished, &hub));
        hub.set_session_active(false);

        assert!(hub.try_begin_recording(0.0).is_some());
        assert!(!notice_relevant(&idle, &hub));
        assert!(!notice_relevant(&finished, &hub));
    }
}
