//! macOS permission status and request bridge for the running app identity.
//!
//! The Swift settings helper has its own executable identity, so it must not
//! query TCC for itself. It writes `.permission-request.json`; this module
//! answers with `permissions.json` from the real dictation process.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use objc2_application_services::{AXIsProcessTrustedWithOptions, kAXTrustedCheckOptionPrompt};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFString, kCFBooleanTrue};
use objc2_core_graphics::{CGPreflightListenEventAccess, CGRequestListenEventAccess};
use serde_json::{json, Map, Value};

use crate::diagnostics::DiagnosticsStore;
use crate::paths;
use crate::timeutil::local_timestamp;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

pub fn microphone_status() -> &'static str {
    let Some(media_type) = (unsafe { AVMediaTypeAudio }) else {
        return "unavailable";
    };
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };
    if status == AVAuthorizationStatus::Authorized {
        "granted"
    } else if status == AVAuthorizationStatus::Denied {
        "denied"
    } else if status == AVAuthorizationStatus::Restricted {
        "restricted"
    } else if status == AVAuthorizationStatus::NotDetermined {
        "not_determined"
    } else {
        "unknown"
    }
}

pub fn accessibility_status() -> &'static str {
    if unsafe { AXIsProcessTrusted() != 0 } {
        "granted"
    } else {
        "missing"
    }
}

pub fn input_monitoring_status() -> &'static str {
    if CGPreflightListenEventAccess() {
        "granted"
    } else {
        "missing"
    }
}

pub fn current_status(permission: &str) -> &'static str {
    match permission {
        "microphone" => microphone_status(),
        "accessibility" => accessibility_status(),
        "input_monitoring" => input_monitoring_status(),
        _ => "unknown",
    }
}

pub fn required_permissions_granted(
    microphone: &str,
    accessibility: &str,
    input_monitoring: &str,
    input_monitoring_required: bool,
) -> bool {
    let mut granted = microphone == "granted" && accessibility == "granted";
    if input_monitoring_required {
        granted = granted && input_monitoring == "granted";
    }
    granted
}

pub fn executable_fingerprint(path: &Path) -> String {
    let Ok(mut file) = fs::File::open(path) else {
        return "unknown".to_string();
    };
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(_) => return "unknown".to_string(),
        }
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(32);
    for byte in digest.iter().take(8) {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

pub fn bundle_identifier(bundle: Option<&Path>) -> String {
    let Some(bundle) = bundle else {
        return "source.rust".to_string();
    };
    let plist = bundle.join("Contents/Info.plist");
    let Ok(text) = fs::read_to_string(&plist) else {
        return "unknown".to_string();
    };
    parse_plist_string(&text, "CFBundleIdentifier").unwrap_or_else(|| "unknown".to_string())
}

fn parse_plist_string(text: &str, key: &str) -> Option<String> {
    let needle = format!("<key>{key}</key>");
    let start = text.find(&needle)? + needle.len();
    let rest = &text[start..];
    let open = rest.find("<string>")? + "<string>".len();
    let close = rest[open..].find("</string>")?;
    Some(
        rest[open..open + close]
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .trim()
            .to_string(),
    )
}

fn python_str(value: Option<&Value>) -> String {
    match value {
        None => "None".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(flag)) => (if *flag { "True" } else { "False" }).to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Null) => "None".to_string(),
        Some(other) => other.to_string(),
    }
}

fn python_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|float| float != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

fn wall_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default()
}

fn settings_active(status_path: &Path) -> bool {
    let instance = status_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".settings-instance");
    let Ok(text) = fs::read_to_string(instance) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<libc::pid_t>() else {
        return false;
    };
    pid > 1 && unsafe { libc::kill(pid, 0) } == 0
}

fn request_microphone() {
    if microphone_status() != "not_determined" {
        return;
    }
    let Some(media_type) = (unsafe { AVMediaTypeAudio }) else {
        return;
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    let block = block2::RcBlock::new(move |_granted: objc2::runtime::Bool| {
        let _ = sender.send(());
    });
    unsafe {
        AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &block);
    }
    let _ = receiver.recv_timeout(Duration::from_secs(60));
}

fn request_accessibility() {
    let prompt_key: &CFString = unsafe { kAXTrustedCheckOptionPrompt };
    let prompt_value: &CFBoolean = unsafe { kCFBooleanTrue }.expect("kCFBooleanTrue");
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(&[prompt_key], &[prompt_value]);
    let options_ref: &CFDictionary = options.as_ref();
    unsafe { AXIsProcessTrustedWithOptions(Some(options_ref)) };
}

fn open_settings_pane(permission: &str) {
    let pane = match permission {
        "microphone" => "Privacy_Microphone",
        "accessibility" => "Privacy_Accessibility",
        "input_monitoring" => "Privacy_ListenEvent",
        _ => return,
    };
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

struct MonitorState {
    last_refresh: f64,
    last_status: Option<Map<String, Value>>,
    last_snapshot: Option<Map<String, Value>>,
}

pub struct PermissionMonitor {
    hotkey_backend: Mutex<String>,
    status_path: PathBuf,
    request_path: PathBuf,
    interval: f64,
    listener_ready: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    identity: Map<String, Value>,
    state: Mutex<MonitorState>,
    stop: AtomicBool,
}

impl PermissionMonitor {
    pub fn new(
        version: &str,
        hotkey_backend: &str,
        status_path: Option<PathBuf>,
        request_path: Option<PathBuf>,
        interval: f64,
        listener_ready: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Self {
        let bundle = paths::app_bundle_path();
        let executable = std::env::current_exe()
            .and_then(|path| path.canonicalize().or(Ok(path)))
            .unwrap_or_default();
        debug_assert!(!executable.as_os_str().is_empty());

        let mut identity = Map::new();
        identity.insert("version".to_string(), json!(version));
        identity.insert(
            "bundle_identifier".to_string(),
            json!(bundle_identifier(bundle.as_deref())),
        );
        identity.insert(
            "bundle_path".to_string(),
            json!(bundle
                .as_ref()
                .unwrap_or(&executable)
                .to_string_lossy()
                .into_owned()),
        );
        identity.insert(
            "executable_path".to_string(),
            json!(executable.to_string_lossy().into_owned()),
        );
        identity.insert(
            "executable_fingerprint".to_string(),
            json!(executable_fingerprint(&executable)),
        );
        identity.insert("pid".to_string(), json!(std::process::id()));

        Self {
            hotkey_backend: Mutex::new(hotkey_backend.to_string()),
            status_path: status_path.unwrap_or_else(paths::permissions_file),
            request_path: request_path.unwrap_or_else(paths::permission_request_file),
            interval: interval.max(0.25),
            listener_ready,
            identity,
            state: Mutex::new(MonitorState {
                last_refresh: f64::NEG_INFINITY,
                last_status: None,
                last_snapshot: None,
            }),
            stop: AtomicBool::new(false),
        }
    }

    pub fn start(self: &Arc<Self>) {
        self.refresh();
        let monitor = Arc::clone(self);
        std::thread::Builder::new()
            .name("permission-monitor".to_string())
            .spawn(move || {
                let interval = Duration::from_secs_f64(monitor.interval);
                while !monitor.stop.load(Ordering::SeqCst) {
                    std::thread::sleep(interval);
                    if monitor.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let _ = monitor.poll();
                }
            })
            .ok();
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn set_hotkey_backend(&self, backend: &str) {
        *self
            .hotkey_backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = backend.to_string();
    }

    pub fn latest_snapshot(&self) -> Option<Map<String, Value>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .last_snapshot
            .clone()
    }

    pub fn poll(&self) -> Result<()> {
        let requested = self.request_path.exists();
        if requested {
            self.consume_request();
        }
        let last_refresh = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .last_refresh;
        let refresh_interval = if settings_active(&self.status_path) {
            self.interval
        } else {
            30.0
        };
        let elapsed = monotonic_seconds() - last_refresh;
        if requested || elapsed >= refresh_interval {
            self.refresh();
        }
        Ok(())
    }

    pub fn refresh(&self) -> Map<String, Value> {
        let microphone = microphone_status();
        let accessibility = accessibility_status();
        let input_monitoring = input_monitoring_status();
        let input_required = self
            .hotkey_backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_str()
            == "passive";

        let mut snapshot = self.identity.clone();
        snapshot.insert("microphone".to_string(), json!(microphone));
        snapshot.insert("accessibility".to_string(), json!(accessibility));
        snapshot.insert("input_monitoring".to_string(), json!(input_monitoring));
        snapshot.insert(
            "input_monitoring_required".to_string(),
            json!(input_required),
        );
        snapshot.insert(
            "all_required_granted".to_string(),
            json!(required_permissions_granted(
                microphone,
                accessibility,
                input_monitoring,
                input_required
            )),
        );
        snapshot.insert("checked_by_current_process".to_string(), json!(true));
        snapshot.insert(
            "hotkey_ready".to_string(),
            json!(self
                .listener_ready
                .as_ref()
                .is_some_and(|ready| ready())),
        );

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.last_status.as_ref() != Some(&snapshot) || !self.status_path.exists() {
            let mut published = snapshot.clone();
            published.insert("updated_at".to_string(), json!(local_timestamp()));
            let _ = DiagnosticsStore::new(Some(self.status_path.clone()), true)
                .save(&Value::Object(published.clone()));
            state.last_status = Some(snapshot);
            state.last_snapshot = Some(published.clone());
            state.last_refresh = monotonic_seconds();
            return published;
        }
        state.last_refresh = monotonic_seconds();
        state.last_snapshot.clone().unwrap_or(snapshot)
    }

    fn consume_request(&self) {
        let Ok(_guard) = self.state.try_lock() else {
            return;
        };
        if !self.request_path.exists() {
            return;
        }
        let parsed = fs::read_to_string(&self.request_path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        let _ = fs::remove_file(&self.request_path);

        let Some(Value::Object(request)) = parsed else {
            return;
        };
        let permission = match request.get("permission") {
            None => "all".to_string(),
            Some(value) => python_str(Some(value)),
        };
        let open_settings = python_truthy(request.get("open_settings"));
        drop(_guard);
        self.request(&permission, open_settings);
    }

    fn request(&self, permission: &str, open_settings: bool) {
        let mut required = vec!["microphone", "accessibility"];
        if self
            .hotkey_backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_str()
            == "passive"
        {
            required.push("input_monitoring");
        }
        let requested: Vec<&str> = if permission == "all" {
            required.clone()
        } else {
            vec![permission]
        };
        for item in requested {
            if !required.contains(&item) || current_status(item) == "granted" {
                continue;
            }
            match item {
                "microphone" => request_microphone(),
                "accessibility" => request_accessibility(),
                "input_monitoring" => {
                    CGRequestListenEventAccess();
                }
                _ => {}
            }
        }
        if open_settings
            && permission != "all"
            && required.contains(&permission)
            && current_status(permission) != "granted"
        {
            open_settings_pane(permission);
        }
    }
}

fn monotonic_seconds() -> f64 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "veyqa-permissions-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn required_matrix_matches_settings_copy() {
        assert!(required_permissions_granted("granted", "granted", "missing", false));
        assert!(!required_permissions_granted("denied", "granted", "granted", false));
        assert!(!required_permissions_granted("granted", "granted", "missing", true));
        assert!(required_permissions_granted("granted", "granted", "granted", true));
    }

    #[test]
    fn fingerprints_are_stable_and_short() {
        let directory = temp_dir();
        let file = directory.join("binary");
        fs::write(&file, b"hello").unwrap();
        assert_eq!(executable_fingerprint(&file), "2cf24dba5fb0a30e");
        assert_eq!(
            executable_fingerprint(&directory.join("missing")),
            "unknown"
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn reads_bundle_identifier_from_xml_plist() {
        let directory = temp_dir();
        let bundle = directory.join("Veyqa Voice.app");
        fs::create_dir_all(bundle.join("Contents")).unwrap();
        fs::write(
            bundle.join("Contents/Info.plist"),
            "<?xml version=\"1.0\"?><plist><dict><key>CFBundleIdentifier</key><string>com.wjcdev.veyqa.rust</string></dict></plist>",
        )
        .unwrap();
        assert_eq!(
            bundle_identifier(Some(&bundle)),
            "com.wjcdev.veyqa.rust"
        );
        assert_eq!(bundle_identifier(None), "source.rust");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn refresh_publishes_the_settings_contract() {
        let directory = temp_dir();
        let status = directory.join("permissions.json");
        let request = directory.join(".permission-request.json");
        let monitor = PermissionMonitor::new(
            "0.1.0",
            "passive",
            Some(status.clone()),
            Some(request),
            2.0,
            Some(Arc::new(|| true)),
        );
        let published = monitor.refresh();
        assert_eq!(published["version"], json!("0.1.0"));
        assert_eq!(published["input_monitoring_required"], json!(true));
        assert_eq!(published["checked_by_current_process"], json!(true));
        assert_eq!(published["hotkey_ready"], json!(true));
        assert!(published["updated_at"].as_str().unwrap().contains('T'));
        assert_eq!(published["pid"], json!(std::process::id()));
        let data: Value =
            serde_json::from_str(&fs::read_to_string(&status).unwrap()).unwrap();
        assert_eq!(data["bundle_identifier"], json!("source.rust"));
        let mode = fs::metadata(&status).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn request_file_is_consumed_even_when_malformed() {
        let directory = temp_dir();
        let status = directory.join("permissions.json");
        let request = directory.join(".permission-request.json");
        let monitor = PermissionMonitor::new(
            "0.1.0",
            "passive",
            Some(status),
            Some(request.clone()),
            2.0,
            None,
        );
        fs::write(&request, "not json").unwrap();
        monitor.poll().unwrap();
        assert!(!request.exists());

        fs::write(&request, "{\"permission\":\"unknown\",\"open_settings\":false}").unwrap();
        monitor.poll().unwrap();
        assert!(!request.exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn settings_instance_pid_gates_the_fast_refresh_interval() {
        let directory = temp_dir();
        let status = directory.join("permissions.json");
        fs::write(directory.join(".settings-instance"), std::process::id().to_string()).unwrap();
        assert!(settings_active(&status));
        fs::write(directory.join(".settings-instance"), "1").unwrap();
        assert!(!settings_active(&status));
        let _ = fs::remove_dir_all(directory);
    }
}
