//! One read-only snapshot of the production `.env`.
//!
//! The Swift settings helper owns every write; this module only resolves the
//! same values Python reads at import time and again when a dictation starts.
//!
//! Developer builds are the one exception: their gesture lives in the
//! launch-proof `.dev-hotkey` file (so no launch path inherits the production
//! key), while the helper only reads `.env`. The two files are mirrored so the
//! settings window always shows the gesture this process actually uses.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::asr::custom::CustomAsrSettings;
use crate::asr::private_trial;
use crate::asr::qwen::QwenConfig;
use crate::asr::speech_models;
use crate::config;
use crate::engine::{EngineConfig, PunctuationMode};
use crate::hotkey::gesture::HotkeyMode;
use crate::hotkey::{display_label, parse_hotkey, HotkeySpec};
use crate::paths;
use crate::release_features;

pub const DEFAULT_HOTKEY: &str = "right_option";
pub const CUSTOM_DEFAULT_HOTKEY: &str = "keycode:49;mods:control+option";
pub const DEFAULT_SETTINGS_BUNDLE_ID: &str = "com.wjcdev.veyqa.settings";
const DEV_SETTINGS_BUNDLE_ID: &str = "com.wjcdev.veyqa.rust.settings";

fn default_settings_bundle_id() -> &'static str {
    if paths::public_build() {
        DEFAULT_SETTINGS_BUNDLE_ID
    } else {
        DEV_SETTINGS_BUNDLE_ID
    }
}

/// `VEYQA_DEV_HOTKEY` wins over the launch-proof `.dev-hotkey` file.
pub fn dev_hotkey_override(env_value: &str, file_value: Option<&str>) -> Option<String> {
    let raw = if env_value.trim().is_empty() {
        file_value.unwrap_or_default()
    } else {
        env_value
    };
    let value = raw.trim().to_lowercase();
    (!value.is_empty()).then_some(value)
}

/// Keep `.dev-hotkey` and the helper-visible `.env` gesture in sync.
///
/// The newer file wins, so editing the shortcut in the settings window (which
/// writes `.env`) is adopted on relaunch, while the dev-only override survives
/// first launches and supervisor restarts.
pub fn sync_dev_hotkey_files() {
    if !paths::is_frozen() || paths::public_build() {
        return;
    }
    let env_path = paths::env_file();
    let file_path = paths::dev_hotkey_file();
    let (Ok(env_text), Ok(file_text)) = (
        fs::read_to_string(&env_path),
        fs::read_to_string(&file_path),
    ) else {
        return;
    };
    let env_spec = read_env_value(&env_text, "VOICE_HOTKEY")
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let file_spec = file_text.trim().to_lowercase();
    if file_spec.is_empty() {
        return;
    }
    if env_spec == file_spec {
        if let Some(label) = settings_hotkey_label(&file_spec) {
            if read_env_value(&env_text, "VOICE_HOTKEY_LABEL").as_deref() != Some(label) {
                let next = set_env_value(&env_text, "VOICE_HOTKEY_LABEL", label);
                let _ = atomic_private_write(&env_path, &next);
            }
        }
        return;
    }
    let env_modified = fs::metadata(&env_path).and_then(|meta| meta.modified()).ok();
    let file_modified = fs::metadata(&file_path)
        .and_then(|meta| meta.modified())
        .ok();
    let adopt_env = match (env_modified, file_modified) {
        (Some(env), Some(file)) => env > file,
        (Some(_), None) => true,
        _ => false,
    };
    if adopt_env && parse_hotkey(&env_spec).is_some() {
        let _ = atomic_private_write(&file_path, &format!("{env_spec}\n"));
    } else {
        write_env_hotkey(&file_spec);
    }
}

/// The settings helper saved a new `.env`; make the dev override follow it
/// before the engine reloads.
pub fn adopt_env_hotkey() {
    if !paths::is_frozen() || paths::public_build() {
        return;
    }
    let Ok(text) = fs::read_to_string(paths::env_file()) else {
        return;
    };
    let Some(spec) = read_env_value(&text, "VOICE_HOTKEY") else {
        return;
    };
    let spec = spec.trim().to_lowercase();
    if parse_hotkey(&spec).is_some() {
        let _ = atomic_private_write(&paths::dev_hotkey_file(), &format!("{spec}\n"));
    }
}

fn write_env_hotkey(spec: &str) {
    let Ok(text) = fs::read_to_string(paths::env_file()) else {
        return;
    };
    let mut next = set_env_value(&text, "VOICE_HOTKEY", spec);
    if let Some(label) = settings_hotkey_label(spec) {
        next = set_env_value(&next, "VOICE_HOTKEY_LABEL", label);
    }
    let _ = atomic_private_write(&paths::env_file(), &next);
}

/// Mirrors the Swift helper's label table so the dev override reads the same
/// way in the settings window.
fn settings_hotkey_label(spec: &str) -> Option<&'static str> {
    Some(match spec {
        "fn" => "地球仪 / Fn（🌐）",
        "left_option" => "左 Option（⌥）",
        "right_option" => "右 Option（⌥）",
        "left_command" => "左 Command（⌘）",
        "right_command" => "右 Command（⌘）",
        "left_control" => "左 Control（⌃）",
        "right_control" => "右 Control（⌃）",
        _ => return None,
    })
}

fn read_env_value(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix(key) else {
            continue;
        };
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        return Some(value.to_string());
    }
    None
}

/// Line-preserving single-key update; comments and every other setting stay
/// byte-for-byte intact.
fn set_env_value(text: &str, key: &str, value: &str) -> String {
    let mut output = String::with_capacity(text.len() + key.len() + value.len() + 2);
    let mut replaced = false;
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = body.trim_start();
        let matches_key = trimmed
            .strip_prefix(key)
            .is_some_and(|rest| rest.starts_with('='));
        if matches_key {
            if !replaced {
                output.push_str(key);
                output.push('=');
                output.push_str(value);
                if line.ends_with('\n') {
                    output.push('\n');
                }
                replaced = true;
            }
            continue;
        }
        output.push_str(line);
    }
    if !replaced {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(key);
        output.push('=');
        output.push_str(value);
        output.push('\n');
    }
    output
}

fn atomic_private_write(path: &Path, text: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings".to_string());
    let temporary = parent.join(format!(".{name}.{}", uuid::Uuid::new_v4().simple()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    if let Err(error) = file.write_all(text.as_bytes()) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct RuntimeSettings {
    /// Normalized `qwen`, `doubao`, or `custom`.
    pub service: String,
    pub engine_label: String,
    pub builtin: bool,
    pub processor_available: bool,
    pub hotkey: HotkeySpec,
    pub hotkey_raw: String,
    pub hotkey_label: String,
    pub hotkey_mode: HotkeyMode,
    pub settings_bundle_id: String,
    pub engine: EngineConfig,
}

impl RuntimeSettings {
    /// What diagnostics reports as `model` after a save.
    pub fn model_name(&self) -> String {
        match self.service.as_str() {
            "qwen" => QwenConfig::from_env()
                .map(|config| config.model)
                .unwrap_or_default(),
            "doubao" => {
                let configured = config::var("DOUBAO_ASR_RESOURCE_ID");
                speech_models::selected_model("doubao", &configured)
                    .unwrap_or_else(|_| configured)
            }
            "custom" => CustomAsrSettings::from_environment().model_name,
            _ => String::new(),
        }
    }

    pub fn load() -> Self {
        let builtin = uses_builtin();
        let configured = normalize_service(&config::var("TRANSCRIPTION_SERVICE"), builtin);
        let service = if builtin {
            "qwen".to_string()
        } else {
            configured
        };
        let engine_label = engine_label(&service).to_string();
        let processor_available = processor_available(&service, builtin);

        let mut hotkey_raw = config::var_or("VOICE_HOTKEY", &default_hotkey()).to_lowercase();
        let hotkey_mode = if config::var("FN_HOTKEY_MODE") == "toggle" {
            HotkeyMode::Toggle
        } else {
            HotkeyMode::Hold
        };
        // Parallel install: let the dev build use a different gesture without
        // touching the production `.env`.
        let file_hotkey = if paths::public_build() {
            None
        } else {
            std::fs::read_to_string(paths::dev_hotkey_file()).ok()
        };
        let dev_hotkey = if paths::public_build() {
            None
        } else {
            dev_hotkey_override(
                &config::var("VEYQA_DEV_HOTKEY"),
                file_hotkey.as_deref(),
            )
        };
        if let Some(dev_hotkey) = dev_hotkey {
            // An unparsable override must not fall back to the production key.
            // Keep `FN_HOTKEY_MODE` so a side-by-side comparison uses the same
            // toggle/hold behaviour as the installed app.
            hotkey_raw = if parse_hotkey(&dev_hotkey).is_some() {
                dev_hotkey
            } else {
                "right_command".to_string()
            };
        }
        let hotkey = parse_hotkey(&hotkey_raw).unwrap_or_else(|| {
            parse_hotkey(DEFAULT_HOTKEY).expect("built-in default hotkey")
        });
        let override_label = config::var("VOICE_HOTKEY_LABEL");
        let hotkey_label = display_label(&hotkey_raw, &override_label);

        let punctuation = PunctuationMode::from_setting(&config::var_or("PUNCTUATION_MODE", "auto"));
        let archive_enabled = config::bool_var("AUDIO_ARCHIVE_ENABLED", false);
        let disfluency_mode = {
            let mode = config::var("DISFLUENCY_FILTER_MODE").to_lowercase();
            (!mode.is_empty()).then_some(mode)
        };
        let disfluency_enabled =
            disfluency_mode.is_none() && config::bool_var("DISFLUENCY_FILTER_ENABLED", false);
        let voice_rms_threshold = env_float("VOICE_RMS_THRESHOLD", 60.0, 20.0);
        let minimum_voiced_ms = env_float("MINIMUM_VOICED_AUDIO_MS", 100.0, 60.0);
        let minimum_total_ms = env_float("MINIMUM_AUDIO_MS", 180.0, 100.0);
        let vocabulary_spelling = crate::custom_vocabulary::spelling_restorer(
            &crate::custom_vocabulary::load_custom_vocabulary(
                Some(&crate::custom_vocabulary::configured_path()),
                crate::custom_vocabulary::MAX_SPELLING_TERMS,
            ),
        )
        .map(std::sync::Arc::new);

        Self {
            engine: EngineConfig {
                engine_label: engine_label.clone(),
                service: service.clone(),
                provider_is_builtin: builtin,
                punctuation,
                disfluency_enabled,
                disfluency_mode,
                hotkey_mode: hotkey_mode_name(hotkey_mode).to_string(),
                hotkey_label: hotkey_label.clone(),
                archive_enabled,
                voice_rms_threshold,
                minimum_total_ms,
                minimum_voiced_ms,
                vocabulary_spelling,
            },
            service,
            engine_label,
            builtin,
            processor_available,
            hotkey,
            hotkey_raw,
            hotkey_label,
            hotkey_mode,
            settings_bundle_id: config::var_or(
                "VEYQA_SETTINGS_BUNDLE_ID",
                default_settings_bundle_id(),
            ),
        }
    }
}

/// Mirrors `transcription.promotion.uses_builtin`.
pub fn uses_builtin() -> bool {
    if !release_features::builtin_speech_enabled() {
        return false;
    }
    let source = config::var("SPEECH_CREDENTIAL_SOURCE");
    if !source.is_empty() {
        return source == "builtin";
    }
    ["QWEN_API_KEY", "DASHSCOPE_API_KEY", "DOUBAO_ACCESS_KEY", "CUSTOM_ASR_MODEL"]
        .iter()
        .all(|key| config::var(key).is_empty())
}

pub fn normalize_service(configured: &str, builtin: bool) -> String {
    let service = configured.trim().to_lowercase();
    if builtin {
        return "qwen".to_string();
    }
    match service.as_str() {
        "qwen" | "doubao" | "custom" => service,
        _ => "qwen".to_string(),
    }
}

pub fn engine_label(service: &str) -> &'static str {
    match service {
        "doubao" => "豆包",
        "custom" => "自定义服务",
        _ => "千问",
    }
}

fn processor_available(service: &str, builtin: bool) -> bool {
    if builtin {
        return private_trial::available();
    }
    match service {
        "doubao" => {
            !config::var("DOUBAO_APP_KEY").is_empty()
                && !config::var("DOUBAO_ACCESS_KEY").is_empty()
        }
        "custom" => CustomAsrSettings::from_environment().is_available(),
        _ => QwenConfig::from_env().is_ok(),
    }
}

/// Python uses the curated chord once the user has a `.env`.
pub fn default_hotkey() -> String {
    if config::config_path().is_file() {
        CUSTOM_DEFAULT_HOTKEY.to_string()
    } else {
        DEFAULT_HOTKEY.to_string()
    }
}

pub fn hotkey_mode_name(mode: HotkeyMode) -> &'static str {
    match mode {
        HotkeyMode::Hold => "hold",
        HotkeyMode::Toggle => "toggle",
    }
}

/// `env_float(name, default, minimum)` from `main.py`.
pub fn env_float(name: &str, default: f64, minimum: f64) -> f64 {
    match config::var(name).parse::<f64>() {
        Ok(value) => value.max(minimum),
        Err(_) => default,
    }
}

/// Where the Swift settings helper lives for this install.
pub fn settings_bundle_id() -> String {
    config::var_or("VEYQA_SETTINGS_BUNDLE_ID", default_settings_bundle_id())
}

pub fn context_file() -> PathBuf {
    crate::paths::context_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_normalize_like_python() {
        assert_eq!(normalize_service("QWEN", false), "qwen");
        assert_eq!(normalize_service(" Doubao ", false), "doubao");
        assert_eq!(normalize_service("custom", false), "custom");
        assert_eq!(normalize_service("whisper", false), "qwen");
        assert_eq!(normalize_service("", false), "qwen");
        assert_eq!(normalize_service("doubao", true), "qwen");
    }

    #[test]
    fn labels_match_the_settings_copy() {
        assert_eq!(engine_label("qwen"), "千问");
        assert_eq!(engine_label("doubao"), "豆包");
        assert_eq!(engine_label("custom"), "自定义服务");
        assert_eq!(engine_label("anything"), "千问");
        assert_eq!(settings_hotkey_label("right_command"), Some("右 Command（⌘）"));
        assert_eq!(settings_hotkey_label("keycode:49;mods:control"), None);
    }

    #[test]
    fn float_overrides_are_clamped_to_the_minimum() {
        // The environment is process-wide; use an unlikely name for the test.
        std::env::set_var("VEYQA_TEST_FLOAT_CLAMP", "5");
        assert_eq!(env_float("VEYQA_TEST_FLOAT_CLAMP", 60.0, 20.0), 20.0);
        std::env::set_var("VEYQA_TEST_FLOAT_CLAMP", "not-a-number");
        assert_eq!(env_float("VEYQA_TEST_FLOAT_CLAMP", 60.0, 20.0), 60.0);
        std::env::remove_var("VEYQA_TEST_FLOAT_CLAMP");
        assert_eq!(env_float("VEYQA_TEST_FLOAT_CLAMP", 60.0, 20.0), 60.0);
    }

    #[test]
    fn env_hotkey_updates_preserve_every_other_line() {
        let original = "# comment\nVOICE_HOTKEY=right_option\nQWEN_API_KEY=secret\n\
                        VOICE_HOTKEY_LABEL=右 Option\n";
        let updated = set_env_value(original, "VOICE_HOTKEY", "right_command");
        assert_eq!(
            updated,
            "# comment\nVOICE_HOTKEY=right_command\nQWEN_API_KEY=secret\n\
             VOICE_HOTKEY_LABEL=右 Option\n"
        );
        assert_eq!(
            read_env_value(&updated, "VOICE_HOTKEY").as_deref(),
            Some("right_command")
        );

        let appended = set_env_value("QWEN_API_KEY=secret", "VOICE_HOTKEY", "fn");
        assert_eq!(appended, "QWEN_API_KEY=secret\nVOICE_HOTKEY=fn\n");
        assert_eq!(read_env_value(&appended, "VOICE_HOTKEY").as_deref(), Some("fn"));

        // Duplicate keys collapse into one canonical line.
        let duplicates = set_env_value("VOICE_HOTKEY=a\nVOICE_HOTKEY=b\n", "VOICE_HOTKEY", "c");
        assert_eq!(duplicates, "VOICE_HOTKEY=c\n");
    }

    #[test]
    fn env_hotkey_reader_ignores_lookalikes_and_unquotes() {
        assert_eq!(
            read_env_value("VOICE_HOTKEY_LABEL=右 Option\n", "VOICE_HOTKEY"),
            None
        );
        assert_eq!(
            read_env_value("VOICE_HOTKEY=\"right_option\"\n", "VOICE_HOTKEY").as_deref(),
            Some("right_option")
        );
    }

    #[test]
    fn default_hotkey_depends_on_the_config_file() {
        let previous = std::env::var_os("VEYQA_CONFIG_FILE");
        std::env::set_var(
            "VEYQA_CONFIG_FILE",
            std::env::temp_dir().join("veyqa-definitely-missing.env"),
        );
        assert_eq!(default_hotkey(), DEFAULT_HOTKEY);
        let path = std::env::temp_dir().join(format!(
            "veyqa-settings-test-{}-{}.env",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, "VOICE_HOTKEY=right_option\n").unwrap();
        std::env::set_var("VEYQA_CONFIG_FILE", &path);
        assert_eq!(default_hotkey(), CUSTOM_DEFAULT_HOTKEY);
        let _ = std::fs::remove_file(path);
        match previous {
            Some(value) => std::env::set_var("VEYQA_CONFIG_FILE", value),
            None => std::env::remove_var("VEYQA_CONFIG_FILE"),
        }
    }

    #[test]
    fn mode_names_feed_the_engine_copy() {
        assert_eq!(hotkey_mode_name(HotkeyMode::Hold), "hold");
        assert_eq!(hotkey_mode_name(HotkeyMode::Toggle), "toggle");
    }

    #[test]
    fn dev_hotkey_env_wins_over_the_file_and_blanks_are_ignored() {
        assert_eq!(
            dev_hotkey_override("right_command", Some("left_command\n")),
            Some("right_command".into())
        );
        assert_eq!(
            dev_hotkey_override("  ", Some(" right_control \n")),
            Some("right_control".into())
        );
        assert_eq!(dev_hotkey_override("", Some("\n")), None);
        assert_eq!(dev_hotkey_override("", None), None);
    }
}
