//! Marker files exchanged with the Swift settings helper and supervisor.
//! Mirrors the request handling in `main.py`.

use std::path::Path;

use crate::paths;

pub const MAX_SETTINGS_REVISION_CHARACTERS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionState {
    Absent,
    /// Empty or over-long revisions are ignored and deliberately left on disk,
    /// exactly like the Python engine.
    Invalid,
    Ready(String),
}

pub fn settings_apply_request() -> std::path::PathBuf {
    paths::settings_apply_request_file()
}

pub fn read_settings_revision(path: &Path) -> RevisionState {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RevisionState::Absent;
        }
        Err(_) => return RevisionState::Invalid,
    };
    let revision = text.trim().to_string();
    if revision.is_empty() || revision.chars().count() > MAX_SETTINGS_REVISION_CHARACTERS {
        RevisionState::Invalid
    } else {
        RevisionState::Ready(revision)
    }
}

/// Consume a settings save only when no newer save arrived while applying it.
pub fn consume_settings_revision(path: &Path, revision: &str) -> bool {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim() == revision => std::fs::remove_file(path).is_ok(),
        _ => false,
    }
}

pub fn marker_requested(path: &Path) -> bool {
    path.exists()
}

/// `unlink(missing_ok=True)` for one-shot request markers.
pub fn consume_marker(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "veyqa-ipc-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn reads_and_trims_valid_revisions() {
        let directory = temp_dir();
        let path = directory.join(".settings-apply-request");
        std::fs::write(&path, "  save-1\n").unwrap();
        assert_eq!(
            read_settings_revision(&path),
            RevisionState::Ready("save-1".into())
        );
        assert_eq!(
            read_settings_revision(&directory.join("missing")),
            RevisionState::Absent
        );
    }

    #[test]
    fn empty_and_over_long_revisions_are_invalid_and_left_on_disk() {
        let directory = temp_dir();
        let empty = directory.join("empty");
        std::fs::write(&empty, " \n").unwrap();
        assert_eq!(read_settings_revision(&empty), RevisionState::Invalid);
        assert!(empty.exists());

        let long = directory.join("long");
        std::fs::write(&long, "x".repeat(MAX_SETTINGS_REVISION_CHARACTERS + 1)).unwrap();
        assert_eq!(read_settings_revision(&long), RevisionState::Invalid);
        assert!(long.exists());

        let at_limit = directory.join("limit");
        std::fs::write(&at_limit, "x".repeat(MAX_SETTINGS_REVISION_CHARACTERS)).unwrap();
        assert!(matches!(
            read_settings_revision(&at_limit),
            RevisionState::Ready(_)
        ));
    }

    #[test]
    fn consumption_keeps_newer_saves() {
        let directory = temp_dir();
        let path = directory.join(".settings-apply-request");

        std::fs::write(&path, "save-1").unwrap();
        assert!(consume_settings_revision(&path, "save-1"));
        assert!(!path.exists());
        assert!(!consume_settings_revision(&path, "save-1"));

        std::fs::write(&path, "save-2").unwrap();
        assert!(!consume_settings_revision(&path, "save-1"));
        assert!(path.exists());
        std::fs::write(&path, " save-2 \n").unwrap();
        assert!(consume_settings_revision(&path, "save-2"));
    }

    #[test]
    fn markers_are_one_shot() {
        let directory = temp_dir();
        let marker = directory.join(".listener-reconnect-request");
        assert!(!marker_requested(&marker));
        std::fs::write(&marker, "").unwrap();
        assert!(marker_requested(&marker));
        consume_marker(&marker);
        assert!(!marker_requested(&marker));
        // Consuming a missing marker is fine.
        consume_marker(&marker);
    }

    #[test]
    fn protocol_paths_live_under_the_data_directory() {
        let data = paths::data_dir();
        assert_eq!(settings_apply_request(), data.join(".settings-apply-request"));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSnapshot {
    pub accessibility: String,
    pub input_monitoring: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectDecision {
    /// Nothing changed; leave the listener alone.
    NoChange,
    /// An utterance is in flight or the device is busy; retry on a later tick.
    Deferred,
    /// Permissions changed in a way that allows a fresh listener binding.
    Rebind,
    /// The snapshot moved on but a passive/registered backend needs nothing.
    Acknowledged,
}

pub struct ReconnectInput<'a> {
    pub current: &'a PermissionSnapshot,
    pub previous: Option<&'a PermissionSnapshot>,
    pub requested: bool,
    pub busy: bool,
    pub hotkey_backend: &'a str,
}

/// Mirror `_refresh_permission_listener`: rebind once after a TCC grant, never
/// restart the app and never poll AX from this path.
pub fn refresh_permission_listener(input: &ReconnectInput<'_>) -> ReconnectDecision {
    let changed = input.previous != Some(input.current);
    if !changed && !input.requested {
        return ReconnectDecision::NoChange;
    }
    if input.busy {
        return ReconnectDecision::Deferred;
    }
    let had_previous = input.previous.is_some();
    let accessibility_granted = input.current.accessibility == "granted";
    let backend_ready = input.hotkey_backend != "passive"
        || input.current.input_monitoring == "granted";
    if (had_previous || input.requested) && accessibility_granted && backend_ready {
        ReconnectDecision::Rebind
    } else {
        ReconnectDecision::Acknowledged
    }
}

/// Build the `execv` argument list for a self-restart.
pub fn restart_arguments(arguments: &[String], show_settings: bool) -> Vec<String> {
    let flag = if show_settings {
        "--show-settings-after-restart"
    } else {
        "--background-login"
    };
    let mut result = vec![flag.to_string()];
    result.extend(
        arguments
            .iter()
            .filter(|argument| {
                argument.as_str() != "--show-settings-after-restart"
                    && argument.as_str() != "--background-login"
            })
            .cloned(),
    );
    result
}

/// The built-in service always speaks the Qwen protocol; anything unknown is Qwen.
pub fn normalize_service(configured: &str, uses_builtin: bool) -> String {
    if uses_builtin {
        return "qwen".to_string();
    }
    match configured.trim().to_lowercase().as_str() {
        "doubao" => "doubao".to_string(),
        "custom" => "custom".to_string(),
        _ => "qwen".to_string(),
    }
}

pub const MICROPHONE_TEST_SECONDS: f64 = 2.0;
/// The Python test compares float samples against 0.002 RMS.
pub const MICROPHONE_TEST_MIN_RMS: f32 = 0.002;

pub fn microphone_test_audible(samples: &[f32]) -> bool {
    if samples.is_empty() {
        return false;
    }
    let mean_square =
        samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32;
    mean_square.sqrt() > MICROPHONE_TEST_MIN_RMS
}

pub fn microphone_test_result(audible: bool) -> &'static str {
    if audible {
        "passed"
    } else {
        "未收到声音，请检查系统声音输入设备和音量"
    }
}

pub const MICROPHONE_TEST_BUSY_STATUS: &str = "请先结束当前听写";
pub const MICROPHONE_TEST_PROGRESS_STATUS: &str = "正在测试，请说一句话（不会上传）";
pub const MICROPHONE_TEST_FAILED_STATUS: &str = "麦克风测试未完成，请检查设备后重试";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrophoneTestDecision {
    /// Absent request, or the test is already running.
    Ignore,
    /// Consume the marker and tell the user to finish the current utterance.
    RejectBusy,
    /// Consume the marker and start the two-second local recording.
    Start,
}

pub fn microphone_test_decision(
    request_exists: bool,
    active: bool,
    busy: bool,
) -> MicrophoneTestDecision {
    if !request_exists || active {
        return MicrophoneTestDecision::Ignore;
    }
    if busy {
        MicrophoneTestDecision::RejectBusy
    } else {
        MicrophoneTestDecision::Start
    }
}

/// Why a settings save was or was not applied on this health tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsApplyDecision {
    /// No request, or an empty/over-long revision that stays on disk.
    Ignore,
    /// The recording lock turned busy after the gate; retry silently next tick.
    DeferRecording,
    /// An utterance/device transition is in flight; report pending.
    Pending(String),
    /// Nothing is busy; reload `.env` and apply the new configuration.
    Apply(String),
}

pub fn settings_apply_decision(
    path: &Path,
    busy: bool,
    recording_locked: bool,
) -> SettingsApplyDecision {
    match read_settings_revision(path) {
        RevisionState::Absent | RevisionState::Invalid => SettingsApplyDecision::Ignore,
        RevisionState::Ready(revision) => {
            if busy {
                SettingsApplyDecision::Pending(revision)
            } else if recording_locked {
                SettingsApplyDecision::DeferRecording
            } else {
                SettingsApplyDecision::Apply(revision)
            }
        }
    }
}

pub fn settings_pending_values(revision: &str) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([
        ("settings_revision".to_string(), serde_json::json!(revision)),
        ("settings_status".to_string(), serde_json::json!("pending")),
    ])
}

pub struct SettingsApplied<'a> {
    pub revision: &'a str,
    pub engine: &'a str,
    pub model: &'a str,
    pub hotkey: &'a str,
    pub login_item_status: &'a str,
    pub login_item_error: &'a str,
}

pub fn settings_applied_values(
    applied: &SettingsApplied<'_>,
) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([
        (
            "settings_revision".to_string(),
            serde_json::json!(applied.revision),
        ),
        (
            "settings_status".to_string(),
            serde_json::json!("applied"),
        ),
        ("engine".to_string(), serde_json::json!(applied.engine)),
        ("model".to_string(), serde_json::json!(applied.model)),
        ("hotkey".to_string(), serde_json::json!(applied.hotkey)),
        (
            "login_item_status".to_string(),
            serde_json::json!(applied.login_item_status),
        ),
        (
            "login_item_error".to_string(),
            serde_json::json!(applied.login_item_error),
        ),
    ])
}

pub fn settings_error_values(revision: &str) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([
        ("settings_revision".to_string(), serde_json::json!(revision)),
        ("settings_status".to_string(), serde_json::json!("error")),
    ])
}

/// Retry only on the next user save, never every health tick.
pub fn finish_settings_apply(path: &Path, revision: &str) -> bool {
    consume_settings_revision(path, revision)
}

#[cfg(test)]
mod settings_lifecycle_tests {
    use super::*;

    fn temp_path() -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "veyqa-settings-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory.join(".settings-apply-request")
    }

    #[test]
    fn settings_decisions_cover_missing_invalid_busy_and_apply() {
        let path = temp_path();
        assert_eq!(
            settings_apply_decision(&path, false, false),
            SettingsApplyDecision::Ignore
        );
        std::fs::write(&path, "   ").unwrap();
        assert_eq!(
            settings_apply_decision(&path, false, false),
            SettingsApplyDecision::Ignore
        );
        std::fs::write(&path, "save-7").unwrap();
        assert_eq!(
            settings_apply_decision(&path, true, false),
            SettingsApplyDecision::Pending("save-7".into())
        );
        assert_eq!(
            settings_apply_decision(&path, false, true),
            SettingsApplyDecision::DeferRecording
        );
        assert_eq!(
            settings_apply_decision(&path, false, false),
            SettingsApplyDecision::Apply("save-7".into())
        );

        assert_eq!(settings_pending_values("save-7")["settings_status"], "pending");
        let applied = settings_applied_values(&SettingsApplied {
            revision: "save-7",
            engine: "qwen",
            model: "qwen-audio",
            hotkey: "右 Option",
            login_item_status: "enabled",
            login_item_error: "",
        });
        assert_eq!(applied["settings_status"], "applied");
        assert_eq!(applied["engine"], "qwen");
        assert_eq!(settings_error_values("save-7")["settings_status"], "error");
    }

    #[test]
    fn finishing_keeps_a_newer_revision_for_the_next_tick() {
        let path = temp_path();
        std::fs::write(&path, "new").unwrap();
        assert!(!finish_settings_apply(&path, "old"));
        assert!(path.exists());
        assert!(finish_settings_apply(&path, "new"));
        assert!(!path.exists());
    }

    #[test]
    fn microphone_test_decisions_match_python() {
        assert_eq!(
            microphone_test_decision(false, false, false),
            MicrophoneTestDecision::Ignore
        );
        assert_eq!(
            microphone_test_decision(true, true, false),
            MicrophoneTestDecision::Ignore
        );
        assert_eq!(
            microphone_test_decision(true, false, true),
            MicrophoneTestDecision::RejectBusy
        );
        assert_eq!(
            microphone_test_decision(true, false, false),
            MicrophoneTestDecision::Start
        );
        assert_eq!(MICROPHONE_TEST_PROGRESS_STATUS, "正在测试，请说一句话（不会上传）");
    }
}

#[cfg(test)]
mod engine_ipc_extra_tests {
    use super::*;

    fn snapshot(accessibility: &str, input_monitoring: &str) -> PermissionSnapshot {
        PermissionSnapshot {
            accessibility: accessibility.to_string(),
            input_monitoring: input_monitoring.to_string(),
        }
    }

    #[test]
    fn permission_listener_only_rebinds_at_a_safe_boundary() {
        let current = snapshot("granted", "granted");
        let previous = snapshot("missing", "missing");
        assert_eq!(
            refresh_permission_listener(&ReconnectInput {
                current: &current,
                previous: Some(&previous),
                requested: false,
                busy: false,
                hotkey_backend: "tap",
            }),
            ReconnectDecision::Rebind
        );
        assert_eq!(
            refresh_permission_listener(&ReconnectInput {
                current: &current,
                previous: Some(&current),
                requested: false,
                busy: false,
                hotkey_backend: "tap",
            }),
            ReconnectDecision::NoChange
        );
        assert_eq!(
            refresh_permission_listener(&ReconnectInput {
                current: &current,
                previous: Some(&previous),
                requested: false,
                busy: true,
                hotkey_backend: "tap",
            }),
            ReconnectDecision::Deferred
        );
    }

    #[test]
    fn passive_backend_needs_input_monitoring_to_rebind() {
        let current = snapshot("granted", "missing");
        assert_eq!(
            refresh_permission_listener(&ReconnectInput {
                current: &current,
                previous: None,
                requested: true,
                busy: false,
                hotkey_backend: "passive",
            }),
            ReconnectDecision::Acknowledged
        );
        assert_eq!(
            refresh_permission_listener(&ReconnectInput {
                current: &current,
                previous: None,
                requested: true,
                busy: false,
                hotkey_backend: "tap",
            }),
            ReconnectDecision::Rebind
        );
    }

    #[test]
    fn reconnect_request_is_acknowledged_when_accessibility_is_missing() {
        let current = snapshot("missing", "granted");
        let previous = snapshot("missing", "missing");
        assert_eq!(
            refresh_permission_listener(&ReconnectInput {
                current: &current,
                previous: Some(&previous),
                requested: true,
                busy: false,
                hotkey_backend: "tap",
            }),
            ReconnectDecision::Acknowledged
        );
    }

    #[test]
    fn restart_arguments_replace_the_old_mode_flag() {
        let arguments = vec![
            "VeyqaVoice".to_string(),
            "--show-settings-after-restart".to_string(),
            "--log".to_string(),
            "/tmp/log".to_string(),
            "--background-login".to_string(),
        ];
        assert_eq!(
            restart_arguments(&arguments, false),
            vec![
                "--background-login".to_string(),
                "VeyqaVoice".to_string(),
                "--log".to_string(),
                "/tmp/log".to_string(),
            ]
        );
        assert_eq!(
            restart_arguments(&[], true),
            vec!["--show-settings-after-restart".to_string()]
        );
    }

    #[test]
    fn service_normalization_matches_python() {
        assert_eq!(normalize_service("doubao", false), "doubao");
        assert_eq!(normalize_service("CUSTOM", false), "custom");
        assert_eq!(normalize_service("qwen", false), "qwen");
        assert_eq!(normalize_service("mystery", false), "qwen");
        assert_eq!(normalize_service("doubao", true), "qwen");
    }

    #[test]
    fn microphone_test_rms_threshold_matches_python() {
        assert!(!microphone_test_audible(&[]));
        assert!(!microphone_test_audible(&[0.0; 4800]));
        assert!(!microphone_test_audible(&[0.001; 4800]));
        assert!(microphone_test_audible(&[0.01; 4800]));
        assert!(microphone_test_audible(&[-0.01; 4800]));
        assert_eq!(microphone_test_result(true), "passed");
        assert!(microphone_test_result(false).contains("未收到声音"));
    }
}
