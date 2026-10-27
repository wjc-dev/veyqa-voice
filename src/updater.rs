//! Main-thread adapter for the native Sparkle controller.
//!
//! Mirrors `src/updater.py` plus the `_UpdateGate` object in
//! `src/ui/status_bar.py`. Only explicit settings commands cross the file
//! boundary; the dylib owns downloads, signatures, and installation.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;

use crate::config;
use crate::diagnostics::DiagnosticsStore;
use crate::paths;
use crate::runtime::hub::StateHub;
use crate::settings;

const MAX_REQUEST_BYTES: u64 = 1024;
const REQUEST_MAX_AGE: f64 = 300.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateAction {
    Check,
    Automatic { enabled: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRequest {
    pub action: UpdateAction,
}

pub struct UpdateCommands {
    path: PathBuf,
    last_id: String,
}

impl UpdateCommands {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            last_id: String::new(),
        }
    }

    pub fn for_data_dir(directory: &Path) -> Self {
        Self::new(directory.join(".update-request.json"))
    }

    pub fn take(&mut self, now: f64) -> Option<UpdateRequest> {
        let metadata = std::fs::symlink_metadata(&self.path).ok()?;
        if metadata.file_type().is_symlink() || metadata.len() > MAX_REQUEST_BYTES {
            return None;
        }
        if owner(&metadata) != Some(unsafe { libc::getuid() }) {
            return None;
        }
        let text = std::fs::read_to_string(&self.path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        let _ = std::fs::remove_file(&self.path);
        let request = value.as_object()?;
        if request
            .keys()
            .any(|key| !matches!(key.as_str(), "id" | "created_at" | "action" | "enabled"))
        {
            return None;
        }
        let identifier = request.get("id")?.as_str()?;
        if identifier.is_empty() || identifier.len() > 64 || identifier == self.last_id {
            return None;
        }
        let created_at = request.get("created_at")?.as_f64()?;
        if !(0.0..=REQUEST_MAX_AGE).contains(&(now - created_at)) {
            return None;
        }
        let action = match request.get("action")?.as_str()? {
            "check" => UpdateAction::Check,
            "automatic" => UpdateAction::Automatic {
                enabled: request.get("enabled")?.as_bool()?,
            },
            _ => return None,
        };
        self.last_id = identifier.to_string();
        Some(UpdateRequest { action })
    }
}

#[cfg(unix)]
fn owner(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.uid())
}

#[cfg(not(unix))]
fn owner(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

/// Ignore stale locks left by a settings crash; never discard a live draft.
pub fn settings_block_install(directory: &Path) -> bool {
    let path = directory.join(".settings-update-state.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return true;
    };
    let Some(pid) = value.get("pid").and_then(serde_json::Value::as_i64) else {
        return true;
    };
    if pid <= 1 {
        return true;
    }
    if !process_exists(pid as i32) {
        return false;
    }
    value.get("dirty") != Some(&serde_json::Value::Bool(false))
}

fn process_exists(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

pub fn unavailable(directory: &Path) {
    let store = DiagnosticsStore::new(Some(directory.join("updates.json")), false);
    let values = serde_json::Map::from_iter([
        (
            "status".to_string(),
            serde_json::json!("暂时无法检查更新，请从官网下载新版"),
        ),
        ("available".to_string(), serde_json::json!(false)),
        ("automatic".to_string(), serde_json::json!(false)),
        ("checking".to_string(), serde_json::json!(false)),
        ("version".to_string(), serde_json::json!("")),
        ("build".to_string(), serde_json::json!("")),
    ]);
    let _ = store.update(&values);
}

struct GateState {
    installing: bool,
    previous_pause: Option<Vec<u8>>,
}

struct GateContext {
    hub: Arc<StateHub>,
    data_dir: PathBuf,
    state: Mutex<GateState>,
}

impl GateContext {
    fn may_check(&self) -> bool {
        let setup_completed = std::env::var("SETUP_COMPLETED")
            .map(|value| value.to_lowercase() == "true")
            .unwrap_or(true);
        let installing = self
            .state
            .lock()
            .map(|state| state.installing)
            .unwrap_or(true);
        !self.hub.busy() && !installing && setup_completed && !settings_block_install(&self.data_dir)
    }

    fn prepare_to_install(&self) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.installing {
            return true;
        }
        if self.hub.busy() || settings_block_install(&self.data_dir) {
            return false;
        }
        let pause = paths::supervisor_paused_file();
        let previous = std::fs::read(&pause).ok();
        if std::fs::write(&pause, "paused-for-update").is_err() {
            return false;
        }
        state.previous_pause = previous;
        state.installing = true;
        true
    }

    fn cancelled(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !state.installing {
            return;
        }
        let pause = paths::supervisor_paused_file();
        match state.previous_pause.take() {
            Some(contents) => {
                let _ = std::fs::write(&pause, contents);
            }
            None => {
                let _ = std::fs::remove_file(&pause);
            }
        }
        state.installing = false;
    }

    fn will_relaunch(&self) {
        terminate_settings_helper();
    }
}

struct GateIvars {
    context: Rc<GateContext>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = GateIvars]
    struct UpdateGate;

    unsafe impl NSObjectProtocol for UpdateGate {}

    impl UpdateGate {
        #[unsafe(method(updateMayCheck))]
        fn may_check(&self) -> Bool {
            Bool::new(self.ivars().context.may_check())
        }

        #[unsafe(method(updatePrepareToInstall))]
        fn prepare_to_install(&self) -> Bool {
            Bool::new(self.ivars().context.prepare_to_install())
        }

        #[unsafe(method(updateDidCancel))]
        fn did_cancel(&self) {
            self.ivars().context.cancelled();
        }

        #[unsafe(method(updateWillRelaunch))]
        fn will_relaunch(&self) {
            self.ivars().context.will_relaunch();
        }
    }
);

impl UpdateGate {
    fn new(mtm: MainThreadMarker, context: Rc<GateContext>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(GateIvars { context });
        unsafe { msg_send![super(this), init] }
    }
}

/// The settings helper is a separate process started by this app.
fn terminate_settings_helper() {
    let Some(bundle) = paths::app_bundle_path() else {
        return;
    };
    let expected = bundle.join("Contents/Helpers/VeyqaSettings.app/Contents/MacOS/VeyqaSettings");
    let expected = std::fs::canonicalize(&expected).unwrap_or(expected);
    let identifier = NSString::from_str(&settings::settings_bundle_id());
    let applications = NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier);
    for index in 0..applications.count() {
        let application = applications.objectAtIndex(index);
        let Some(url) = application.executableURL() else {
            continue;
        };
        let Some(path) = url.path() else {
            continue;
        };
        let path = PathBuf::from(path.to_string());
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if path == expected {
            let _ = application.terminate();
        }
    }
}

pub struct AppUpdater {
    _library: *mut libc::c_void,
    controller: Retained<AnyObject>,
    _gate: Retained<UpdateGate>,
    commands: UpdateCommands,
}

// The controller is only ever touched on the AppKit main thread.

impl AppUpdater {
    pub fn load(
        mtm: MainThreadMarker,
        bundle: &Path,
        data_dir: &Path,
        hub: Arc<StateHub>,
    ) -> Result<Self, String> {
        let library_path = bundle.join("Contents/Frameworks/VeyqaUpdates.dylib");
        let library_path = CString::new(library_path.as_os_str().as_encoded_bytes())
            .map_err(|_| "无法初始化应用更新".to_string())?;
        let library = unsafe { libc::dlopen(library_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        if library.is_null() {
            return Err("无法初始化应用更新".to_string());
        }
        let Some(class) = AnyClass::get(c"VeyqaUpdateController") else {
            unsafe { libc::dlclose(library) };
            return Err("无法初始化应用更新".to_string());
        };

        let context = Rc::new(GateContext {
            hub,
            data_dir: data_dir.to_path_buf(),
            state: Mutex::new(GateState {
                installing: false,
                previous_pause: None,
            }),
        });
        let gate = UpdateGate::new(mtm, context);

        let bundle_string = NSString::from_str(&bundle.to_string_lossy());
        let data_string = NSString::from_str(&data_dir.to_string_lossy());
        let alloc: *mut AnyObject = unsafe { msg_send![class, alloc] };
        let controller: *mut AnyObject = unsafe {
            msg_send![
                alloc,
                initWithBundlePath: &*bundle_string,
                dataPath: &*data_string,
                gate: &*gate
            ]
        };
        let Some(controller) = (unsafe { Retained::from_raw(controller) }) else {
            unsafe { libc::dlclose(library) };
            return Err("无法初始化应用更新".to_string());
        };
        let _: () = unsafe { msg_send![&*controller, start] };

        Ok(Self {
            _library: library,
            controller,
            _gate: gate,
            commands: UpdateCommands::for_data_dir(data_dir),
        })
    }

    pub fn check(&self) {
        let _: () = unsafe { msg_send![&*self.controller, checkForUpdates] };
    }

    pub fn set_automatic(&self, enabled: bool) {
        let _: () = unsafe { msg_send![&*self.controller, setAutomaticChecks: Bool::new(enabled)] };
    }

    pub fn resume_if_ready(&self) {
        let _: () = unsafe { msg_send![&*self.controller, resumeIfReady] };
    }

    pub fn tick(&mut self) {
        if let Some(request) = self.commands.take(now()) {
            match request.action {
                UpdateAction::Check => self.check(),
                UpdateAction::Automatic { enabled } => self.set_automatic(enabled),
            }
        }
        self.resume_if_ready();
    }
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

/// Mirrors Python's `IS_FROZEN` gate around the native controller.
pub fn available() -> bool {
    paths::is_frozen()
        && !config::bool_var("VEYQA_DISABLE_UPDATER", false)
        && paths::app_bundle_path()
            .map(|bundle| bundle.join("Contents/Frameworks/VeyqaUpdates.dylib").is_file())
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn request_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "veyqa-updates-{}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_request(dir: &Path, value: serde_json::Value) {
        let mut file = std::fs::File::create(dir.join(".update-request.json")).unwrap();
        file.write_all(value.to_string().as_bytes()).unwrap();
    }

    #[test]
    fn takes_one_valid_check_request_and_removes_the_file() {
        let dir = request_file("check");
        write_request(
            &dir,
            serde_json::json!({"id": "abc", "created_at": 100.0, "action": "check"}),
        );
        let mut commands = UpdateCommands::for_data_dir(&dir);
        assert_eq!(
            commands.take(120.0),
            Some(UpdateRequest {
                action: UpdateAction::Check
            })
        );
        assert!(!dir.join(".update-request.json").exists());
        // The same id never replays.
        write_request(
            &dir,
            serde_json::json!({"id": "abc", "created_at": 130.0, "action": "check"}),
        );
        assert_eq!(commands.take(140.0), None);
    }

    #[test]
    fn automatic_requests_require_a_real_boolean() {
        let dir = request_file("automatic");
        write_request(
            &dir,
            serde_json::json!({"id": "a1", "created_at": 10.0, "action": "automatic", "enabled": true}),
        );
        let mut commands = UpdateCommands::for_data_dir(&dir);
        assert_eq!(
            commands.take(20.0),
            Some(UpdateRequest {
                action: UpdateAction::Automatic { enabled: true }
            })
        );

        write_request(
            &dir,
            serde_json::json!({"id": "a2", "created_at": 10.0, "action": "automatic", "enabled": 1}),
        );
        assert_eq!(commands.take(20.0), None);
    }

    #[test]
    fn rejects_stale_foreign_and_malformed_requests() {
        let dir = request_file("reject");
        let mut commands = UpdateCommands::for_data_dir(&dir);
        for value in [
            serde_json::json!({"id": "x", "created_at": -400.0, "action": "check"}),
            serde_json::json!({"id": "", "created_at": 10.0, "action": "check"}),
            serde_json::json!({"id": "x", "created_at": 10.0, "action": "install"}),
            serde_json::json!({"id": "x", "created_at": 10.0, "action": "check", "extra": 1}),
            serde_json::json!({"id": "x", "created_at": true, "action": "check"}),
        ] {
            write_request(&dir, value);
            assert_eq!(commands.take(20.0), None);
        }
        std::fs::write(dir.join(".update-request.json"), "not json").unwrap();
        assert_eq!(commands.take(20.0), None);
    }

    #[test]
    fn oversized_or_symlinked_requests_are_ignored() {
        let dir = request_file("oversized");
        let path = dir.join(".update-request.json");
        std::fs::write(&path, vec![b' '; 2048]).unwrap();
        let mut commands = UpdateCommands::for_data_dir(&dir);
        assert_eq!(commands.take(20.0), None);

        std::fs::remove_file(&path).unwrap();
        let target = dir.join("target.json");
        std::fs::write(&target, "{}").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(commands.take(20.0), None);
    }

    #[test]
    fn settings_drafts_block_only_live_dirty_editors() {
        let dir = request_file("settings");
        let state = dir.join(".settings-update-state.json");

        assert!(!settings_block_install(&dir));

        std::fs::write(&state, format!("{{\"pid\": {}}}", i32::MAX)).unwrap();
        assert!(!settings_block_install(&dir));

        std::fs::write(&state, format!("{{\"pid\": {}, \"dirty\": true}}", std::process::id())).unwrap();
        assert!(settings_block_install(&dir));

        std::fs::write(
            &state,
            format!("{{\"pid\": {}, \"dirty\": false}}", std::process::id()),
        )
        .unwrap();
        assert!(!settings_block_install(&dir));

        std::fs::write(&state, "{\"pid\": 0}").unwrap();
        assert!(settings_block_install(&dir));

        std::fs::write(&state, "not json").unwrap();
        assert!(settings_block_install(&dir));
    }

    #[test]
    fn unavailable_writes_the_shared_status_file() {
        let dir = request_file("unavailable");
        unavailable(&dir);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("updates.json")).unwrap()).unwrap();
        assert_eq!(value["status"], "暂时无法检查更新，请从官网下载新版");
        assert_eq!(value["available"], false);
        assert_eq!(value["checking"], false);
    }
}
