//! Per-utterance session state and the orchestration gates from `main.py`.
//!
//! The Python app serializes everything with an `RLock` and a per-session
//! `RLock`; the Rust engine runs the same bookkeeping behind a mutex so that
//! stale callbacks can be identified by session identity and ignored.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Map, Value};

use crate::state::InputState;

pub const STALLED_TRANSITION_REASON: &str = "录音设备操作超时";
pub const PROCESSING_TIMEOUT_REASON: &str = "识别结束超时";
pub const LISTENER_STALL_REASON: &str = "快捷键监听循环无响应";

#[derive(Debug)]
struct SessionState {
    cancelled: bool,
    final_emitted: bool,
    completed: bool,
    latest_preview: String,
    archive_path: Option<PathBuf>,
    preview_count: u64,
    first_preview_ms: Option<i64>,
    total_audio_ms: i64,
    voiced_audio_ms: i64,
    final_committed: bool,
    recovery_saved: bool,
    outcome: String,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            cancelled: false,
            final_emitted: false,
            completed: false,
            latest_preview: String::new(),
            archive_path: None,
            preview_count: 0,
            first_preview_ms: None,
            total_audio_ms: 0,
            voiced_audio_ms: 0,
            final_committed: false,
            recovery_saved: false,
            outcome: "recording".to_string(),
        }
    }
}

/// All mutable state belonging to exactly one push-to-talk utterance.
pub struct VoiceSession<T = ()> {
    pub session_id: String,
    pub output_target: Option<T>,
    pub started_at: f64,
    state: Mutex<SessionState>,
}

impl<T> VoiceSession<T> {
    pub fn new(session_id: impl Into<String>, output_target: Option<T>, started_at: f64) -> Self {
        Self {
            session_id: session_id.into(),
            output_target,
            started_at,
            state: Mutex::new(SessionState::default()),
        }
    }

    fn state(&self) -> MutexGuard<'_, SessionState> {
        // A panicking callback must not wedge session bookkeeping for good.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn is_cancelled(&self) -> bool {
        self.state().cancelled
    }

    pub fn is_completed(&self) -> bool {
        self.state().completed
    }

    pub fn is_active(&self) -> bool {
        let state = self.state();
        !state.cancelled && !state.completed
    }

    pub fn cancel(&self) {
        self.state().cancelled = true;
    }

    /// Returns true only for the first caller; duplicate completions are no-ops.
    pub fn mark_completed(&self) -> bool {
        let mut state = self.state();
        if state.completed {
            return false;
        }
        state.completed = true;
        true
    }

    pub fn latest_preview(&self) -> String {
        self.state().latest_preview.clone()
    }

    pub fn preview_count(&self) -> u64 {
        self.state().preview_count
    }

    pub fn first_preview_ms(&self) -> Option<i64> {
        self.state().first_preview_ms
    }

    pub fn audio_ms(&self) -> (i64, i64) {
        let state = self.state();
        (state.total_audio_ms, state.voiced_audio_ms)
    }

    pub fn record_audio(&self, total_audio_ms: f64, voiced_audio_ms: f64) {
        let mut state = self.state();
        state.total_audio_ms = total_audio_ms as i64;
        state.voiced_audio_ms = voiced_audio_ms as i64;
    }

    pub fn record_preview(&self, text: impl Into<String>, elapsed_ms: i64) {
        let mut state = self.state();
        state.latest_preview = text.into();
        state.preview_count += 1;
        if state.first_preview_ms.is_none() {
            state.first_preview_ms = Some(elapsed_ms);
        }
    }

    /// Reserve the single final-commit slot. Late duplicates return false.
    pub fn begin_final(&self) -> bool {
        let mut state = self.state();
        if state.cancelled || state.final_emitted {
            return false;
        }
        state.final_emitted = true;
        true
    }

    pub fn set_final_committed(&self, committed: bool) {
        self.state().final_committed = committed;
    }

    pub fn final_committed(&self) -> bool {
        self.state().final_committed
    }

    pub fn final_emitted(&self) -> bool {
        self.state().final_emitted
    }

    /// Atomically take the preview for the recovery store.
    ///
    /// Cancelled content and already-handled results never qualify. The claim
    /// is marked before the write so retries cannot duplicate the entry; a
    /// failed write releases the claim via `release_recovery_claim`.
    pub fn claim_recovery(&self) -> Option<String> {
        let mut state = self.state();
        if state.cancelled
            || state.final_emitted
            || state.recovery_saved
            || state.latest_preview.is_empty()
        {
            return None;
        }
        state.recovery_saved = true;
        Some(state.latest_preview.clone())
    }

    pub fn release_recovery_claim(&self) {
        self.state().recovery_saved = false;
    }

    pub fn set_archive_path(&self, path: Option<PathBuf>) {
        self.state().archive_path = path;
    }

    pub fn archive_path(&self) -> Option<PathBuf> {
        self.state().archive_path.clone()
    }

    pub fn set_outcome(&self, outcome: &str) {
        self.state().outcome = outcome.to_string();
    }

    pub fn outcome(&self) -> String {
        self.state().outcome.clone()
    }

    /// Non-content quality signals shared with the settings diagnostics page.
    pub fn diagnostics(&self) -> Map<String, Value> {
        let state = self.state();
        let mut values = Map::new();
        values.insert(
            "last_session_outcome".to_string(),
            json!(state.outcome),
        );
        values.insert("last_session_audio_ms".to_string(), json!(state.total_audio_ms));
        values.insert(
            "last_session_voiced_ms".to_string(),
            json!(state.voiced_audio_ms),
        );
        values.insert(
            "last_session_preview_count".to_string(),
            json!(state.preview_count),
        );
        values.insert(
            "last_session_first_preview_ms".to_string(),
            json!(state.first_preview_ms),
        );
        values.insert(
            "last_session_committed".to_string(),
            json!(state.final_committed),
        );
        values
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginReject {
    AlreadyActive,
    PreviousStreamAlive,
}

/// Serializes ownership: only one session may own the microphone at a time.
pub struct SessionRegistry<T = ()> {
    current: Option<Arc<VoiceSession<T>>>,
    finished_session_id: Option<String>,
}

impl<T> Default for SessionRegistry<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> SessionRegistry<T> {
    pub fn new() -> Self {
        Self {
            current: None,
            finished_session_id: None,
        }
    }

    pub fn current(&self) -> Option<Arc<VoiceSession<T>>> {
        self.current.clone()
    }

    pub fn has_active_session(&self) -> bool {
        self.current.is_some()
    }

    pub fn is_current(&self, session: &Arc<VoiceSession<T>>) -> bool {
        self.current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, session))
    }

    pub fn try_begin(
        &mut self,
        session: Arc<VoiceSession<T>>,
        previous_stream_alive: bool,
    ) -> Result<Arc<VoiceSession<T>>, BeginReject> {
        if self.current.is_some() {
            return Err(BeginReject::AlreadyActive);
        }
        if previous_stream_alive {
            return Err(BeginReject::PreviousStreamAlive);
        }
        self.current = Some(session.clone());
        Ok(session)
    }

    /// Close only the session that still owns the microphone.
    ///
    /// An old callback finishing late must never stop the next session.
    pub fn finish(&mut self, session: &Arc<VoiceSession<T>>) -> bool {
        if !self.is_current(session) {
            return false;
        }
        if !session.mark_completed() {
            return false;
        }
        self.current = None;
        self.finished_session_id = Some(session.session_id.clone());
        true
    }

    /// Mark the owned session cancelled and return it for teardown.
    pub fn begin_cancel(&mut self) -> Option<Arc<VoiceSession<T>>> {
        let session = self.current.clone()?;
        session.cancel();
        Some(session)
    }

    /// Async notices must not leak into the next utterance.
    pub fn notice_is_relevant(&self, session_id: &str, recording: bool) -> bool {
        self.current.is_none()
            && self.finished_session_id.as_deref() == Some(session_id)
            && !recording
    }
}

/// Every reason `start_streaming` can decline a new utterance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartReject {
    MicrophoneTestActive,
    Unconfigured { engine_label: String },
    UpdateInstalling,
    TransitionCancelled,
    AlreadyActive,
    PreviousStreamAlive,
    SettingsFocused,
}

impl StartReject {
    pub fn notice(&self) -> Option<String> {
        match self {
            Self::MicrophoneTestActive => Some("麦克风正在测试，请稍候再开始听写".to_string()),
            Self::Unconfigured { engine_label } => Some(format!(
                "请打开 Veyqa Voice 设置，连接{engine_label}并保存，再开始听写"
            )),
            Self::UpdateInstalling => {
                Some("正在安装更新，即将重新打开，请稍候".to_string())
            }
            Self::SettingsFocused => {
                Some("请先点击试用文本框，或其他 App 的输入框，再使用语音快捷键".to_string())
            }
            Self::TransitionCancelled
            | Self::AlreadyActive
            | Self::PreviousStreamAlive => None,
        }
    }

    /// Silent cancellations must not double-reset keyboard state.
    pub fn resets_keyboard(&self) -> bool {
        !matches!(self, Self::TransitionCancelled)
    }
}

pub struct StartConditions<'a> {
    pub microphone_test_active: bool,
    pub processor_available: bool,
    pub update_installing: bool,
    pub transition_cancelled: bool,
    pub session_active: bool,
    pub previous_stream_alive: bool,
    pub frontmost_settings: bool,
    pub setup_trial_target: bool,
    pub engine_label: &'a str,
}

pub fn start_gate(conditions: &StartConditions<'_>) -> Result<(), StartReject> {
    if conditions.microphone_test_active {
        return Err(StartReject::MicrophoneTestActive);
    }
    if !conditions.processor_available {
        return Err(StartReject::Unconfigured {
            engine_label: conditions.engine_label.to_string(),
        });
    }
    if conditions.update_installing {
        return Err(StartReject::UpdateInstalling);
    }
    if conditions.transition_cancelled {
        return Err(StartReject::TransitionCancelled);
    }
    if conditions.session_active {
        return Err(StartReject::AlreadyActive);
    }
    if conditions.previous_stream_alive {
        return Err(StartReject::PreviousStreamAlive);
    }
    if conditions.frontmost_settings && !conditions.setup_trial_target {
        return Err(StartReject::SettingsFocused);
    }
    Ok(())
}

/// Only a deliberate foreground launch may open the settings window.
pub fn should_open_settings_on_start(
    processor_present: bool,
    _is_frozen: bool,
    arguments: &[String],
    restarting: bool,
    setup_complete: bool,
) -> bool {
    if arguments.iter().any(|value| value == "--show-settings-after-restart") {
        return true;
    }
    if restarting || arguments.iter().any(|value| value == "--background-login") {
        return false;
    }
    !processor_present || !setup_complete
}

pub struct HealthInput {
    pub transition_started_at: Option<f64>,
    pub state: InputState,
    pub state_since: f64,
    pub listener_last_poll: Option<f64>,
}

pub fn health_fault(now: f64, input: &HealthInput) -> Option<&'static str> {
    if input
        .transition_started_at
        .is_some_and(|started| now - started > 10.0)
    {
        return Some(STALLED_TRANSITION_REASON);
    }
    if input.state == InputState::Processing && now - input.state_since > 45.0 {
        return Some(PROCESSING_TIMEOUT_REASON);
    }
    if input
        .listener_last_poll
        .is_some_and(|polled| now - polled > 15.0)
    {
        return Some(LISTENER_STALL_REASON);
    }
    None
}

pub fn update_busy(
    session_active: bool,
    microphone_test_active: bool,
    recording: bool,
    transition_pending: bool,
    streaming_alive: bool,
) -> bool {
    session_active
        || microphone_test_active
        || recording
        || transition_pending
        || streaming_alive
}

pub fn update_may_check(
    busy: bool,
    installing: bool,
    setup_completed: bool,
    settings_blocking: bool,
) -> bool {
    !busy && !installing && setup_completed && !settings_blocking
}

/// A settings apply waits for the current utterance and still reports pending.
pub fn settings_apply_pending(
    session_active: bool,
    microphone_test_active: bool,
    streaming_alive: bool,
    transition_pending: bool,
) -> bool {
    session_active || microphone_test_active || streaming_alive || transition_pending
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, preview: &str) -> Arc<VoiceSession<()>> {
        let session = Arc::new(VoiceSession::new(id, None, 0.0));
        if !preview.is_empty() {
            session.record_preview(preview, 10);
        }
        session
    }

    #[test]
    fn duplicate_final_callbacks_commit_exactly_once() {
        let session = session("s1", "只应输入一次");
        let mut typed = Vec::new();
        for _ in 0..2 {
            if session.begin_final() {
                typed.push("只应输入一次");
            }
        }
        session.set_final_committed(!typed.is_empty());
        session.set_outcome("committed");
        assert_eq!(typed, ["只应输入一次"]);
        let values = session.diagnostics();
        assert_eq!(values["last_session_outcome"], json!("committed"));
        assert_eq!(values["last_session_committed"], json!(true));
        assert!(values["last_session_preview_count"].as_u64().unwrap() >= 1);
        assert!(!format!("{values:?}").contains("只应输入一次"));
    }

    #[test]
    fn cancelled_session_never_commits_late_callback() {
        let session = session("s1", "不该提交");
        session.cancel();
        assert!(!session.begin_final());
        session.set_outcome("cancelled");
        assert_eq!(session.diagnostics()["last_session_committed"], json!(false));
    }

    #[test]
    fn unexpected_exception_retains_preview_once() {
        let session = session("s1", "原话仍然在这里");
        assert_eq!(
            session.claim_recovery().as_deref(),
            Some("原话仍然在这里")
        );
        assert_eq!(session.claim_recovery(), None);
    }

    #[test]
    fn cancelled_or_already_emitted_text_is_not_recovered() {
        let cancelled = session("a", "不应重复保存");
        cancelled.cancel();
        assert_eq!(cancelled.claim_recovery(), None);

        let emitted = session("b", "不应重复保存");
        assert!(emitted.begin_final());
        assert_eq!(emitted.claim_recovery(), None);

        let empty = session("c", "");
        assert_eq!(empty.claim_recovery(), None);
    }

    #[test]
    fn failed_recovery_write_can_be_retried() {
        let session = session("s1", "要保存的文字");
        assert!(session.claim_recovery().is_some());
        session.release_recovery_claim();
        assert_eq!(session.claim_recovery().as_deref(), Some("要保存的文字"));
    }

    #[test]
    fn user_cancel_never_retains_partial_text() {
        let mut registry = SessionRegistry::new();
        let active = session("s1", "取消后不应保留");
        registry.try_begin(active.clone(), false).unwrap();
        let cancelled = registry.begin_cancel().unwrap();
        assert!(cancelled.is_cancelled());
        assert_eq!(cancelled.claim_recovery(), None);
    }

    #[test]
    fn old_session_cleanup_does_not_affect_the_new_session() {
        let mut registry = SessionRegistry::new();
        let old = session("old", "");
        let current = session("current", "");
        registry.try_begin(current.clone(), false).unwrap();
        assert!(!registry.finish(&old));
        assert!(registry.is_current(&current));
    }

    #[test]
    fn finishing_the_owned_session_clears_it_and_scopes_notices() {
        let mut registry = SessionRegistry::new();
        let current = session("current", "");
        registry.try_begin(current.clone(), false).unwrap();
        assert!(registry.finish(&current));
        assert!(!registry.has_active_session());
        assert!(registry.notice_is_relevant("current", false));
        assert!(!registry.notice_is_relevant("current", true));
        assert!(!registry.notice_is_relevant("other", false));
        assert!(!registry.finish(&current));
    }

    #[test]
    fn start_rejects_duplicate_sessions_and_previous_streams() {
        let mut registry = SessionRegistry::new();
        let first = session("a", "");
        registry.try_begin(first.clone(), false).unwrap();
        assert_eq!(
            registry.try_begin(session("b", ""), false).err(),
            Some(BeginReject::AlreadyActive)
        );

        let mut registry = SessionRegistry::new();
        assert_eq!(
            registry.try_begin(session("c", ""), true).err(),
            Some(BeginReject::PreviousStreamAlive)
        );
    }

    fn conditions() -> StartConditions<'static> {
        StartConditions {
            microphone_test_active: false,
            processor_available: true,
            update_installing: false,
            transition_cancelled: false,
            session_active: false,
            previous_stream_alive: false,
            frontmost_settings: false,
            setup_trial_target: false,
            engine_label: "千问",
        }
    }

    #[test]
    fn start_gate_ready_path_is_quiet() {
        assert_eq!(start_gate(&conditions()), Ok(()));
    }

    #[test]
    fn start_gate_maps_every_rejection_to_python_feedback() {
        let mut input = conditions();
        input.microphone_test_active = true;
        assert_eq!(
            start_gate(&input),
            Err(StartReject::MicrophoneTestActive)
        );
        assert_eq!(
            StartReject::MicrophoneTestActive.notice().as_deref(),
            Some("麦克风正在测试，请稍候再开始听写")
        );

        let mut input = conditions();
        input.processor_available = false;
        let reject = start_gate(&input).unwrap_err();
        assert_eq!(
            reject.notice().as_deref(),
            Some("请打开 Veyqa Voice 设置，连接千问并保存，再开始听写")
        );

        let mut input = conditions();
        input.update_installing = true;
        assert_eq!(start_gate(&input), Err(StartReject::UpdateInstalling));

        let mut input = conditions();
        input.transition_cancelled = true;
        let reject = start_gate(&input).unwrap_err();
        assert!(!reject.resets_keyboard());
        assert_eq!(reject.notice(), None);

        let mut input = conditions();
        input.session_active = true;
        assert_eq!(start_gate(&input), Err(StartReject::AlreadyActive));

        let mut input = conditions();
        input.previous_stream_alive = true;
        assert_eq!(
            start_gate(&input),
            Err(StartReject::PreviousStreamAlive)
        );

        let mut input = conditions();
        input.frontmost_settings = true;
        assert_eq!(start_gate(&input), Err(StartReject::SettingsFocused));

        let mut input = conditions();
        input.frontmost_settings = true;
        input.setup_trial_target = true;
        assert_eq!(start_gate(&input), Ok(()));
    }

    #[test]
    fn launch_policy_matches_python() {
        let manual = vec!["VeyqaVoice".to_string()];
        let background = vec!["VeyqaVoice".to_string(), "--background-login".to_string()];
        let explicit = vec![
            "VeyqaVoice".to_string(),
            "--show-settings-after-restart".to_string(),
        ];

        assert!(should_open_settings_on_start(false, true, &manual, false, true));
        assert!(!should_open_settings_on_start(true, true, &manual, false, true));
        assert!(should_open_settings_on_start(true, true, &manual, false, false));
        assert!(!should_open_settings_on_start(true, true, &background, false, false));
        assert!(!should_open_settings_on_start(false, true, &manual, true, true));
        assert!(should_open_settings_on_start(true, true, &explicit, true, true));
    }

    fn health(
        transition: Option<f64>,
        state: InputState,
        state_since: f64,
        polled: Option<f64>,
    ) -> HealthInput {
        HealthInput {
            transition_started_at: transition,
            state,
            state_since,
            listener_last_poll: polled,
        }
    }

    #[test]
    fn stalled_transition_wins_over_other_faults() {
        let input = health(Some(100.0), InputState::Idle, 110.0, Some(110.0));
        assert_eq!(health_fault(111.0, &input), Some(STALLED_TRANSITION_REASON));
        assert_eq!(health_fault(110.0, &input), None);
    }

    #[test]
    fn processing_times_out_after_forty_five_seconds() {
        let input = health(None, InputState::Processing, 100.0, Some(145.5));
        assert_eq!(health_fault(145.0, &input), None);
        assert_eq!(
            health_fault(145.5, &input),
            Some(PROCESSING_TIMEOUT_REASON)
        );
        let idle = health(None, InputState::Idle, 0.0, Some(1000.0));
        assert_eq!(health_fault(1000.0, &idle), None);
    }

    #[test]
    fn listener_poll_gap_is_a_fault_only_when_known() {
        let input = health(None, InputState::Idle, 200.0, Some(200.0));
        assert_eq!(health_fault(215.0, &input), None);
        assert_eq!(health_fault(215.5, &input), Some(LISTENER_STALL_REASON));
        let unknown = health(None, InputState::Idle, 200.0, None);
        assert_eq!(health_fault(1000.0, &unknown), None);
    }

    #[test]
    fn busy_and_update_gates_match_python() {
        assert!(update_busy(true, false, false, false, false));
        assert!(update_busy(false, true, false, false, false));
        assert!(update_busy(false, false, true, false, false));
        assert!(update_busy(false, false, false, true, false));
        assert!(update_busy(false, false, false, false, true));
        assert!(!update_busy(false, false, false, false, false));

        assert!(!update_may_check(true, false, true, false));
        assert!(!update_may_check(false, true, true, false));
        assert!(!update_may_check(false, false, false, false));
        assert!(!update_may_check(false, false, true, true));
        assert!(update_may_check(false, false, true, false));

        assert!(settings_apply_pending(true, false, false, false));
        assert!(settings_apply_pending(false, true, false, false));
        assert!(settings_apply_pending(false, false, true, false));
        assert!(settings_apply_pending(false, false, false, true));
        assert!(!settings_apply_pending(false, false, false, false));
    }

    #[test]
    fn preview_metrics_keep_first_timestamp_and_count_every_update() {
        let session: VoiceSession<()> = VoiceSession::new("s", None, 0.0);
        session.record_preview("a", 120);
        session.record_preview("ab", 240);
        assert_eq!(session.preview_count(), 2);
        assert_eq!(session.first_preview_ms(), Some(120));
        assert_eq!(session.latest_preview(), "ab");

        session.record_audio(1000.4, 800.9);
        assert_eq!(session.audio_ms(), (1000, 800));
    }
}
