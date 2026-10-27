use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// `VEYQA_DISTRIBUTION=public` builds are the in-place replacement and share
/// the production Python data directory; every other build stays parallel.
const DEV_SUPPORT_DIR: &str = "Veyqa-Rust-Dev";
const PUBLIC_SUPPORT_DIR: &str = "Veyqa";

pub fn public_build() -> bool {
    matches!(option_env!("VEYQA_DISTRIBUTION"), Some("public"))
}

/// Version reported to the Swift helper, which compares it with the bundle's
/// `CFBundleShortVersionString`. Release builds pass the packaged version so
/// the public app reports 0.3.x instead of the crate's internal 0.1.x.
pub fn app_version() -> &'static str {
    option_env!("VEYQA_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

fn support_dir() -> &'static str {
    if public_build() {
        PUBLIC_SUPPORT_DIR
    } else {
        DEV_SUPPORT_DIR
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn expand_path(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(value)
}

pub fn is_frozen() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| app_bundle_of(&path))
        .is_some()
}

pub fn app_bundle_path() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    app_bundle_of(&executable)
}

fn app_bundle_of(executable: &Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .map(Path::to_path_buf)
}

pub fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn resource_root() -> PathBuf {
    if let Some(root) = std::env::var_os("VOICE_INPUT_ROOT") {
        return PathBuf::from(root);
    }
    if let Some(bundle) = app_bundle_path() {
        return bundle.join("Contents/Resources");
    }
    source_root()
}

pub fn data_dir() -> PathBuf {
    if let Some(override_dir) = std::env::var_os("VOICE_INPUT_DATA_DIR") {
        return expand_path(&override_dir.to_string_lossy());
    }
    if is_frozen() {
        home_dir()
            .join("Library/Application Support")
            .join(support_dir())
    } else {
        source_root()
    }
}

pub fn env_file() -> PathBuf {
    data_dir().join(".env")
}

/// Dev-build hotkey override. A supervisor or login-item relaunch starts the
/// app without our environment, so the parallel install keeps its gesture in
/// a file instead of `VEYQA_DEV_HOTKEY`.
pub fn dev_hotkey_file() -> PathBuf {
    data_dir().join(".dev-hotkey")
}

pub fn context_file() -> PathBuf {
    data_dir().join("personal_context.txt")
}

pub fn custom_vocabulary_file() -> PathBuf {
    data_dir().join("custom_vocabulary.txt")
}

pub fn corrections_file() -> PathBuf {
    data_dir().join("corrections.json")
}

pub fn recovery_file() -> PathBuf {
    data_dir().join("recovery.json")
}

pub fn diagnostics_file() -> PathBuf {
    data_dir().join("diagnostics.json")
}

pub fn permissions_file() -> PathBuf {
    data_dir().join("permissions.json")
}

pub fn permission_request_file() -> PathBuf {
    data_dir().join(".permission-request.json")
}

pub fn audio_archive_dir() -> PathBuf {
    data_dir().join("audio_archive")
}

pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

pub fn settings_apply_request_file() -> PathBuf {
    data_dir().join(".settings-apply-request")
}

pub fn microphone_test_request_file() -> PathBuf {
    data_dir().join(".microphone-test-request")
}

pub fn listener_reconnect_request_file() -> PathBuf {
    data_dir().join(".listener-reconnect-request")
}

pub fn shortcut_capture_file() -> PathBuf {
    data_dir().join(".shortcut-capture")
}

pub fn settings_instance_file() -> PathBuf {
    data_dir().join(".settings-instance")
}

pub fn settings_update_state_file() -> PathBuf {
    data_dir().join(".settings-update-state.json")
}

pub fn update_request_file() -> PathBuf {
    data_dir().join(".update-request.json")
}

pub fn updates_file() -> PathBuf {
    data_dir().join("updates.json")
}

pub fn health_file() -> PathBuf {
    data_dir().join("health.json")
}

pub fn supervisor_paused_file() -> PathBuf {
    data_dir().join(".supervisor-paused")
}

pub fn promotion_file() -> PathBuf {
    data_dir().join("promotion.json")
}

pub fn promotion_usage_file() -> PathBuf {
    data_dir().join("promotion-usage.json")
}

pub fn promotion_prefetch_request_file() -> PathBuf {
    data_dir().join(".promotion-prefetch-request")
}

pub fn restart_request_file() -> PathBuf {
    data_dir().join(".restart-request")
}

pub fn app_instance_lock_file() -> PathBuf {
    data_dir().join(".app-instance.lock")
}

const ENV_TEMPLATE: &str = "TRANSCRIPTION_SERVICE=qwen\n\
QWEN_API_KEY=\n\
VOICE_HOTKEY=right_option\n\
VOICE_HOTKEY_LABEL=右 Option（⌥）\n\
FN_HOTKEY_MODE=hold\n\
GLOBAL_HOTKEY_BACKEND=passive\n\
SHOW_MENU_BAR_ICON=true\n\
SHOW_DOCK_ICON=false\n\
PUNCTUATION_MODE=spaces\n\
AUDIO_ARCHIVE_ENABLED=false\n\
DISFLUENCY_FILTER_ENABLED=false\n\
DISFLUENCY_FILTER_MODE=off\n\
EXPERIMENTAL_CORRECTION_LEARNING=false\n\
CORRECTION_LEARNING_ENABLED=false\n\
CORRECTION_AUTO_REPLACE=false\n\
CORRECTION_REPLACE_MIN_COUNT=2\n\
CORRECTION_CONTEXT_ENABLED=false\n\
CORRECTION_CONTEXT_MIN_COUNT=2\n";

pub fn ensure_runtime_layout() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let data = data_dir();
    if is_frozen() && !data.exists() {
        for legacy in [
            home_dir().join("Library/Application Support/Voice Input Next"),
            home_dir().join("Library/Application Support/VoxType Next"),
            home_dir().join("Library/Application Support/VoxType"),
        ] {
            if !legacy.is_dir() {
                continue;
            }
            std::fs::create_dir_all(&data)?;
            for name in [
                ".env",
                "personal_context.txt",
                "custom_vocabulary.txt",
                "corrections.json",
                "recovery.json",
            ] {
                let source = legacy.join(name);
                let destination = data.join(name);
                if source.is_file() && !destination.exists() {
                    std::fs::copy(&source, &destination).with_context(|| {
                        format!("无法迁移 {} 到 {}", source.display(), destination.display())
                    })?;
                }
            }
            break;
        }
    }

    std::fs::create_dir_all(&data)
        .with_context(|| format!("无法创建数据目录 {}", data.display()))?;
    let _ = std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700));
    std::fs::create_dir_all(log_dir())
        .with_context(|| format!("无法创建日志目录 {}", log_dir().display()))?;

    if is_frozen() && !env_file().exists() {
        let template = resource_root().join("env.example");
        if template.is_file() {
            std::fs::copy(&template, env_file()).with_context(|| {
                format!("无法复制配置模板 {}", template.display())
            })?;
        } else {
            std::fs::write(env_file(), ENV_TEMPLATE)
                .with_context(|| format!("无法写入 {}", env_file().display()))?;
        }
        let _ = std::fs::set_permissions(env_file(), std::fs::Permissions::from_mode(0o600));
    }

    // Never inherit the production gesture: every launch path of this build
    // (Finder, login item, supervisor, self-restart) reads the same file.
    if is_frozen() && !public_build() && !dev_hotkey_file().exists() {
        if std::fs::write(dev_hotkey_file(), "right_command\n").is_ok() {
            let _ = std::fs::set_permissions(
                dev_hotkey_file(),
                std::fs::Permissions::from_mode(0o600),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_paths() {
        let expanded = expand_path("~/Library/Test");
        assert_eq!(expanded, home_dir().join("Library/Test"));
        assert_eq!(expand_path("/tmp/x"), PathBuf::from("/tmp/x"));
    }

    #[test]
    fn detects_app_bundle_ancestors() {
        let path = Path::new("/Applications/Veyqa Voice.app/Contents/MacOS/veyqa-voice-rs");
        assert_eq!(
            app_bundle_of(path),
            Some(PathBuf::from("/Applications/Veyqa Voice.app"))
        );
        assert_eq!(app_bundle_of(Path::new("/tmp/veyqa")), None);
    }

    #[test]
    fn dev_runs_use_the_source_root() {
        if std::env::var_os("VOICE_INPUT_DATA_DIR").is_none() {
            assert_eq!(data_dir(), source_root());
        }
    }

    #[test]
    fn exposes_protocol_paths_under_the_data_dir() {
        let data = data_dir();
        assert_eq!(env_file(), data.join(".env"));
        assert_eq!(recovery_file(), data.join("recovery.json"));
        assert_eq!(permission_request_file(), data.join(".permission-request.json"));
        assert_eq!(dev_hotkey_file(), data.join(".dev-hotkey"));
    }
}
