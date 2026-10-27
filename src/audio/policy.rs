//! Recording lifecycle constants and failure classification.
//!
//! Mirrors the timing policy in `audio/recorder.py` and the supervisor
//! timeouts in `audio/isolated_recorder.py`, without touching a device.

pub const DEFAULT_POST_ROLL_MS: u32 = 120;
pub const MAX_POST_ROLL_MS: u32 = 300;

/// A blocked device start must not trap the hotkey forever.
pub const START_TIMEOUT_SECONDS: f64 = 4.0;
/// All audio delivered before a stalled stop remains usable.
pub const STOP_TIMEOUT_SECONDS: f64 = 1.5;
/// Ten minutes, then submit what was captured.
pub const MAX_RECORD_SECONDS: f64 = 600.0;

pub const WORKER_ENV_ALLOWLIST: [&str; 6] = [
    "HOME",
    "PATH",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "VOICE_INPUT_DATA_DIR",
];

pub const WORKER_SPAWN_ERROR: &str = "录音工作进程无法启动，请检查系统是否拦截应用";
pub const START_TIMEOUT_ERROR: &str = "麦克风准备超时，本次录音已取消；请检查设备后重试";
pub const CANCELLED_ERROR: &str = "本次录音已取消";
pub const WORKER_INTERRUPTED_ERROR: &str = "录音工作进程中断，请重试";
pub const WORKER_EXITED_ERROR: &str = "录音工作进程已退出";

/// `AUDIO_POST_ROLL_MS`: clamped to 0..=300, malformed values keep 120.
pub fn post_roll_ms_from_str(value: &str) -> u32 {
    match value.trim().parse::<i64>() {
        Ok(millis) => millis.clamp(0, MAX_POST_ROLL_MS as i64) as u32,
        Err(_) => DEFAULT_POST_ROLL_MS,
    }
}

pub fn post_roll_ms() -> u32 {
    let raw = crate::config::var("AUDIO_POST_ROLL_MS");
    if raw.is_empty() {
        DEFAULT_POST_ROLL_MS
    } else {
        post_roll_ms_from_str(&raw)
    }
}

/// PortAudio status strings: an input/device failure counts, an overflow does not.
pub fn is_device_error_status(status: &str) -> bool {
    let status = status.to_lowercase();
    (status.contains("input") || status.contains("device")) && !status.contains("overflow")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_roll_is_clamped_and_malformed_values_fall_back() {
        assert_eq!(post_roll_ms_from_str("120"), 120);
        assert_eq!(post_roll_ms_from_str(" 0 "), 0);
        assert_eq!(post_roll_ms_from_str("-40"), 0);
        assert_eq!(post_roll_ms_from_str("9999"), 300);
        assert_eq!(post_roll_ms_from_str("fast"), DEFAULT_POST_ROLL_MS);
        assert_eq!(post_roll_ms_from_str(""), DEFAULT_POST_ROLL_MS);
    }

    #[test]
    fn worker_environment_never_forwards_secrets() {
        assert_eq!(WORKER_ENV_ALLOWLIST.len(), 6);
        assert!(!WORKER_ENV_ALLOWLIST.contains(&"QWEN_API_KEY"));
        assert!(!WORKER_ENV_ALLOWLIST.contains(&"DASHSCOPE_API_KEY"));
        assert!(!WORKER_ENV_ALLOWLIST.contains(&"VOICE_INPUT_BUNDLED"));
    }

    #[test]
    fn only_input_or_device_status_is_a_disconnect() {
        assert!(is_device_error_status(
            "AudioUnit: input device unavailable"
        ));
        assert!(is_device_error_status("DEVICE LOST"));
        assert!(!is_device_error_status("input overflow"));
        assert!(!is_device_error_status("output underflow"));
        assert!(!is_device_error_status(""));
    }

    #[test]
    fn timeout_and_duration_constants_match_python() {
        assert_eq!(START_TIMEOUT_SECONDS, 4.0);
        assert_eq!(STOP_TIMEOUT_SECONDS, 1.5);
        assert_eq!(MAX_RECORD_SECONDS, 600.0);
    }
}
