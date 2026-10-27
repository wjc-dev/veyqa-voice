//! Modern login-item registration for the bundled app.
//!
//! Mirrors `src/login_item.py`: the LaunchAgent supervises the tiny bundled
//! helper rather than the app itself, so login recovery survives an app that
//! refuses to start. Registration always goes through `SMAppService`; this
//! module never writes launchd plists.

use std::path::Path;
use std::process::{Command, Stdio};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_app_kit::NSRunningApplication;
use objc2_foundation::{NSBundle, NSString};

use crate::config;
use crate::paths;

// Class lookup needs the framework image in the process; AppKit does not
// load ServiceManagement on its own.
#[link(name = "ServiceManagement", kind = "framework")]
extern "C" {}

/// The production app registers `com.wjcdev.veyqa.*`; the parallel install
/// keeps its own agent so both builds can coexist.
pub const AGENT_PLIST_NAME: &str = "com.wjcdev.veyqa.rust.agent.plist";
pub const LOCAL_AGENT_PLIST_NAME: &str = "com.wjcdev.veyqa.rust.local.agent.plist";
pub const PUBLIC_AGENT_PLIST_NAME: &str = "com.wjcdev.veyqa.agent.plist";
pub const PUBLIC_LOCAL_AGENT_PLIST_NAME: &str = "com.wjcdev.veyqa.local.agent.plist";

const LEGACY_AGENT_NAMES: &[&str] = &[
    "com.voxtype.dev.agent.plist",
    "com.voxtype.dev.plist",
    "com.voxtype.dev.legacy.plist",
    "com.voiceinputnext.qwen.plist",
    "com.voiceinputnext.app.plist",
    "com.whisper-input-next.plist",
];

const LEGACY_BUNDLE_IDS: &[&str] = &["com.voiceinputnext.qwen", "com.voiceinputnext.app"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginItemResult {
    pub status: String,
    pub changed: bool,
    pub error: String,
}

impl LoginItemResult {
    fn new(status: impl Into<String>, changed: bool, error: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            changed,
            error: error.into(),
        }
    }
}

impl Default for LoginItemResult {
    fn default() -> Self {
        Self::new("not_registered", false, "")
    }
}

/// A Finder/Spotlight launch resumes service after an explicit user quit.
pub fn resume_supervisor_for_manual_launch(arguments: &[String]) {
    if arguments.iter().any(|argument| argument == "--supervised") {
        return;
    }
    let _ = std::fs::remove_file(paths::supervisor_paused_file());
}

pub fn sync_login_item(enabled: bool) -> LoginItemResult {
    if disabled_for_test() {
        return LoginItemResult::new("disabled_for_test", false, "");
    }
    if !paths::is_frozen() {
        return LoginItemResult::new("source_run", false, "");
    }
    let Some(bundle) = paths::app_bundle_path() else {
        return LoginItemResult::new("error", false, "无法定位应用包");
    };
    if AnyClass::get(c"SMAppService").is_none() {
        return LoginItemResult::new("error", false, "当前系统不支持后台启动服务");
    }
    let plist_name = agent_plist_name(bundle_profile(&bundle).as_deref());
    let identity = registration_identity(&bundle, &bundle_version(&bundle));
    let identity_changed = !identity.is_empty() && identity != registered_identity();
    let result = sync_native(enabled, plist_name, &bundle);
    if enabled && identity_changed && result.status == "enabled" {
        schedule_agent_repair(plist_name);
    }
    result
}

/// Replacing a bundle leaves launchd/BTM with a spawn attempt built against the
/// old app record; the first registration after an update can be recorded as
/// enabled while its supervisor never starts. A second registration after BTM
/// settles clears that stale spawn.
fn schedule_agent_repair(plist_name: &'static str) {
    let _ = std::thread::Builder::new()
        .name("login-agent-repair".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let _ = unregister(plist_name);
            std::thread::sleep(std::time::Duration::from_millis(400));
            let _ = register(plist_name);
        });
}

fn disabled_for_test() -> bool {
    for name in ["VEYQA_DISABLE_LOGIN_SYNC", "VOICE_INPUT_DISABLE_LOGIN_SYNC"] {
        if config::bool_var(name, false) {
            return true;
        }
    }
    false
}

pub fn agent_plist_name(profile: Option<&str>) -> &'static str {
    if paths::public_build() {
        return if profile == Some("local") {
            PUBLIC_LOCAL_AGENT_PLIST_NAME
        } else {
            PUBLIC_AGENT_PLIST_NAME
        };
    }
    if profile == Some("local") {
        LOCAL_AGENT_PLIST_NAME
    } else {
        AGENT_PLIST_NAME
    }
}

pub fn status_name(value: isize) -> String {
    match value {
        0 => "not_registered".to_string(),
        1 => "enabled".to_string(),
        2 => "requires_approval".to_string(),
        3 => "not_found".to_string(),
        other => format!("unknown_{other}"),
    }
}

/// Replacing a .app can leave `SMAppService` attached to the old bundle inode,
/// even while its status still reads `enabled`.
pub fn registration_identity(bundle: &Path, version: &str) -> String {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(bundle) {
        Ok(metadata) => format!(
            "{}:{}:{}:{}",
            bundle.display(),
            version,
            metadata.dev(),
            metadata.ino()
        ),
        Err(_) => String::new(),
    }
}

fn sync_native(enabled: bool, plist_name: &str, bundle: &Path) -> LoginItemResult {
    if plist_name == LOCAL_AGENT_PLIST_NAME {
        // BTM can retain a launch constraint from an older ad-hoc identity.
        // A distinct local service carries its own approval instead.
        match service_status(AGENT_PLIST_NAME) {
            Ok(status) if matches!(status_name(status).as_str(), "enabled" | "requires_approval") => {
                if let Err(error) = unregister(AGENT_PLIST_NAME) {
                    return LoginItemResult::new(
                        "error",
                        false,
                        prefixed(error, "停用旧签名后台服务失败"),
                    );
                }
            }
            _ => {}
        }
    }

    let mut before = match service_status(plist_name) {
        Ok(status) => status_name(status),
        Err(error) => return LoginItemResult::new("error", false, error),
    };
    let mut changed = false;

    if enabled {
        let identity = registration_identity(bundle, &bundle_version(bundle));
        let previous = registered_identity();
        if !identity.is_empty()
            && identity != previous
            && matches!(before.as_str(), "enabled" | "requires_approval")
        {
            if let Err(error) = unregister(plist_name) {
                return LoginItemResult::new(&before, false, prefixed(error, "更新后台服务失败"));
            }
            before = "not_registered".to_string();
            changed = true;
        }
        if !matches!(before.as_str(), "enabled" | "requires_approval") {
            if let Err(error) = register(plist_name) {
                return LoginItemResult::new(&before, false, prefixed(error, "注册失败"));
            }
            changed = true;
        }
        let after = match service_status(plist_name) {
            Ok(status) => status_name(status),
            Err(error) => return LoginItemResult::new("error", changed, error),
        };
        if !identity.is_empty() && matches!(after.as_str(), "enabled" | "requires_approval") {
            let _ = write_registration_identity(&identity);
        }
        if after == "enabled" {
            retire_legacy_agents();
            if legacy_app_is_running() {
                return LoginItemResult::new(
                    "enabled_legacy_running",
                    changed,
                    "检测到旧版 Voice Input 仍在运行；请先退出旧版，避免两个快捷键监听器同时工作。",
                );
            }
        }
        return LoginItemResult::new(after, changed, "");
    }

    if !matches!(before.as_str(), "not_registered" | "not_found") {
        if let Err(error) = unregister(plist_name) {
            return LoginItemResult::new(&before, false, prefixed(error, "取消注册失败"));
        }
        changed = true;
    }
    let after = match service_status(plist_name) {
        Ok(status) => status_name(status),
        Err(error) => return LoginItemResult::new("error", changed, error),
    };
    LoginItemResult::new(after, changed, "")
}

fn prefixed(error: String, fallback: &str) -> String {
    if error.is_empty() {
        fallback.to_string()
    } else {
        error
    }
}

fn bundle_version(bundle: &Path) -> String {
    let _ = bundle;
    let Some(info) = bundle_info() else {
        return String::new();
    };
    info.objectForKey(&NSString::from_str("CFBundleVersion"))
        .and_then(|value| value.downcast::<NSString>().ok())
        .map(|value| value.to_string())
        .unwrap_or_default()
}

fn bundle_profile(bundle: &Path) -> Option<String> {
    let _ = bundle;
    let info = bundle_info()?;
    info.objectForKey(&NSString::from_str("VeyqaSigningProfile"))
        .and_then(|value| value.downcast::<NSString>().ok())
        .map(|value| value.to_string())
}

fn bundle_info() -> Option<Retained<objc2_foundation::NSDictionary<NSString, AnyObject>>> {
    let bundle = NSBundle::mainBundle();
    bundle.infoDictionary()
}

fn registered_identity() -> String {
    registered_identity_at(&paths::data_dir().join("login-item-registration.json"))
}

fn registered_identity_at(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return String::new();
    };
    value
        .get("identity")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn write_registration_identity(identity: &str) -> std::io::Result<()> {
    let path = paths::data_dir().join("login-item-registration.json");
    std::fs::write(path, serde_json::json!({ "identity": identity }).to_string())
}

fn retire_legacy_agents() {
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let directory = paths::home_dir().join("Library/LaunchAgents");
    for name in LEGACY_AGENT_NAMES {
        let path = directory.join(name);
        if !path.exists() {
            continue;
        }
        let _ = Command::new("/bin/launchctl")
            .arg("bootout")
            .arg(&domain)
            .arg(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_file(&path);
    }
}

fn legacy_app_is_running() -> bool {
    LEGACY_BUNDLE_IDS.iter().any(|identifier| {
        let identifier = NSString::from_str(identifier);
        let applications = NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier);
        applications.count() > 0
    })
}

fn service_status(plist_name: &str) -> Result<isize, String> {
    let service = agent_service(plist_name)?;
    let status: isize = unsafe { msg_send![&*service, status] };
    Ok(status)
}

fn register(plist_name: &str) -> Result<(), String> {
    let service = agent_service(plist_name)?;
    let mut error: *mut AnyObject = std::ptr::null_mut();
    let ok: bool = unsafe {
        msg_send![&*service, registerAndReturnError: &mut error as *mut *mut AnyObject]
    };
    if ok {
        Ok(())
    } else {
        Err(describe_error(error).unwrap_or_default())
    }
}

fn unregister(plist_name: &str) -> Result<(), String> {
    let service = agent_service(plist_name)?;
    let mut error: *mut AnyObject = std::ptr::null_mut();
    let ok: bool = unsafe {
        msg_send![&*service, unregisterAndReturnError: &mut error as *mut *mut AnyObject]
    };
    if ok {
        Ok(())
    } else {
        Err(describe_error(error).unwrap_or_default())
    }
}

fn describe_error(error: *mut AnyObject) -> Option<String> {
    if error.is_null() {
        return None;
    }
    let description: Option<Retained<NSString>> =
        unsafe { msg_send![error, localizedDescription] };
    description.map(|description| description.to_string())
}

fn agent_service(plist_name: &str) -> Result<Retained<AnyObject>, String> {
    let class = AnyClass::get(c"SMAppService")
        .ok_or_else(|| "当前系统不支持后台启动服务".to_string())?;
    let name = NSString::from_str(plist_name);
    // `agentServiceWithPlistName:` follows the none method family, so the
    // returned instance is autoreleased; `Option<Retained<_>>` makes objc2
    // retain it. Taking the raw pointer with `from_raw` would over-release it.
    let service: Option<Retained<AnyObject>> =
        unsafe { msg_send![class, agentServiceWithPlistName: &*name] };
    service.ok_or_else(|| "无法初始化后台启动服务".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_bundles_get_a_distinct_agent() {
        assert_eq!(agent_plist_name(Some("local")), LOCAL_AGENT_PLIST_NAME);
        assert_eq!(agent_plist_name(Some("release")), AGENT_PLIST_NAME);
        assert_eq!(agent_plist_name(None), AGENT_PLIST_NAME);
        assert_eq!(PUBLIC_LOCAL_AGENT_PLIST_NAME, "com.wjcdev.veyqa.local.agent.plist");
        assert_eq!(PUBLIC_AGENT_PLIST_NAME, "com.wjcdev.veyqa.agent.plist");
    }

    #[test]
    fn status_names_match_the_settings_copy() {
        assert_eq!(status_name(0), "not_registered");
        assert_eq!(status_name(1), "enabled");
        assert_eq!(status_name(2), "requires_approval");
        assert_eq!(status_name(3), "not_found");
        assert_eq!(status_name(42), "unknown_42");
    }

    #[test]
    fn identity_embeds_the_bundle_path_version_and_inode() {
        let identity = registration_identity(&paths::source_root(), "1.2.3");
        assert!(identity.starts_with(&format!("{}:1.2.3:", paths::source_root().display())));
        assert_eq!(registration_identity(Path::new("/definitely/missing.app"), "1"), "");
    }

    #[test]
    fn dev_builds_skip_registration() {
        // Tests link the bare executable, so the production path stays inert.
        let result = sync_login_item(true);
        let expected = if disabled_for_test() {
            "disabled_for_test"
        } else {
            "source_run"
        };
        assert_eq!(result.status, expected);
        assert!(!result.changed);
    }
}
