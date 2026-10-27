//! Dictation session orchestration, mirroring `VoiceAssistant` in `main.py`.
//!
//! The engine owns one utterance at a time and never touches AppKit, the
//! clipboard, or the network itself. UI, persistence, and AX insertion arrive
//! through `EngineHost`; microphone control through `AudioCapture`. Provider
//! callbacks are addressed by session id so a late WebSocket message can never
//! leak into the next utterance.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use crate::audio::capture::CaptureEvent;
use crate::audio::gate::VoiceGate;
use crate::custom_vocabulary::SpellingRestorer;
use crate::session::{
    start_gate, BeginReject, SessionRegistry, StartConditions, StartReject, VoiceSession,
};
use crate::setup_feedback::empty_result_notice;
use crate::text::{
    clean_spoken_disfluencies, format_transcription_text, has_sufficient_speech_with,
    is_context_echo, sanitize_inline_text,
};
use crate::ui::NoticeScope;

const RECOVERY_HINT: &str = "；已识别片段可在恢复页复制";
const RECOVERY_NOTICE: &str = "已保留识别片段，请打开恢复页复制";
const LISTENING_TEXT: &str = "正在听写…";
const PROCESSING_TEXT: &str = "正在完成识别…";
const PROVIDER_QUEUE_CHUNKS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PunctuationMode {
    Auto,
    Spaces,
    None,
}

impl PunctuationMode {
    pub fn from_setting(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "spaces" => Self::Spaces,
            "none" => Self::None,
            _ => Self::Auto,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Spaces => "spaces",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub engine_label: String,
    /// `qwen`, `doubao`, or `custom`. Built-in promotion still reports `qwen`.
    pub service: String,
    pub provider_is_builtin: bool,
    pub punctuation: PunctuationMode,
    pub disfluency_enabled: bool,
    pub disfluency_mode: Option<String>,
    pub hotkey_mode: String,
    pub hotkey_label: String,
    pub archive_enabled: bool,
    pub voice_rms_threshold: f64,
    pub minimum_total_ms: f64,
    pub minimum_voiced_ms: f64,
    /// Deterministic spelling restoration for identifier-like hotwords.
    pub vocabulary_spelling: Option<Arc<SpellingRestorer>>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            engine_label: "千问".to_string(),
            service: "qwen".to_string(),
            provider_is_builtin: false,
            punctuation: PunctuationMode::Auto,
            disfluency_enabled: false,
            disfluency_mode: None,
            hotkey_mode: "hold".to_string(),
            hotkey_label: "右 Option".to_string(),
            archive_enabled: false,
            voice_rms_threshold: 60.0,
            minimum_total_ms: 180.0,
            minimum_voiced_ms: 100.0,
            vocabulary_spelling: None,
        }
    }
}

/// Every UI/persistence side effect a session can produce.
pub trait EngineHost<T> {
    fn show_preparing(&mut self);
    fn show_listening(&mut self);
    fn show_processing(&mut self);
    fn update_preview(&mut self, text: &str);
    fn update_level(&mut self, level: f64);
    fn hide_preview(&mut self);
    fn show_notice(&mut self, notice: &str, scope: NoticeScope);
    fn show_warning(&mut self, notice: &str);
    fn show_error(&mut self, message: &str);
    fn mark_streaming(&mut self);
    fn reset_keyboard(&mut self);
    fn commit_text(&mut self, text: &str, target: Option<&T>) -> bool;
    fn retain_recovery(&mut self, text: &str, reason: &str) -> bool;
    fn diagnostics(&mut self, values: &Map<String, Value>);
    fn archive_audio(&mut self, audio: &[u8]) -> Option<PathBuf>;
    fn save_transcript(&mut self, archive_path: &Path, text: &str);
}

/// Microphone capture with the same start/stop contract as `AudioRecorder`.
pub trait AudioCapture {
    fn start(&mut self) -> Result<(), String>;
    fn stop(&mut self, abort: bool) -> Option<Vec<u8>>;
    fn reset_streaming_state(&mut self);
    fn current_device(&self) -> Option<String>;
    fn sample_rate(&self) -> Option<u32>;
    fn capture_error(&self) -> String;
    /// Chunks and lifecycle notices produced since the last call. Defaults to
    /// nothing so tests and platform-free captures stay simple.
    fn poll_events(&mut self) -> Vec<CaptureEvent> {
        Vec::new()
    }
}

pub struct StartRequest<T> {
    pub now: f64,
    pub target: Option<T>,
    pub processor_available: bool,
    pub recognition_context: String,
    pub frontmost_settings: bool,
    pub setup_trial_target: bool,
    pub transition_cancelled: bool,
    pub update_installing: bool,
}

pub struct ProviderStream {
    pub session_id: String,
    /// 16 kHz mono PCM16 little-endian chunks, fed by `VoiceEngine::audio_chunk`.
    pub audio: mpsc::Receiver<Vec<u8>>,
}

#[derive(Debug)]
pub enum StartOutcome {
    Started(ProviderStream),
    Rejected(StartReject),
    CaptureFailed { reason: String },
    Cancelled,
}

/// Callbacks a provider task sends back to the engine, always scoped by session.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    Preview {
        session_id: String,
        text: String,
        at: f64,
    },
    Final {
        session_id: String,
        text: String,
    },
    Complete {
        session_id: String,
    },
    Error {
        session_id: String,
        message: String,
    },
    /// The provider future returned, successfully or not.
    Finished {
        session_id: String,
    },
}

impl std::fmt::Debug for ProviderStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderStream")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

pub struct VoiceEngine<T, H, C> {
    config: EngineConfig,
    host: H,
    capture: C,
    registry: SessionRegistry<T>,
    microphone_test_active: bool,
    provider_audio: Option<(String, mpsc::Sender<Vec<u8>>)>,
    /// Stays set until the provider future has actually returned, so a quick
    /// second press cannot start while the old connection is still draining.
    provider_session_id: Option<String>,
    voice: VoiceGate,
    recognition_context: String,
    stream_failed: bool,
}

impl<T, H, C> VoiceEngine<T, H, C>
where
    T: Clone,
    H: EngineHost<T>,
    C: AudioCapture,
{
    pub fn new(config: EngineConfig, host: H, capture: C) -> Self {
        let voice = VoiceGate::new(
            config.voice_rms_threshold,
            config.minimum_total_ms,
            config.minimum_voiced_ms,
        );
        Self {
            config,
            host,
            capture,
            registry: SessionRegistry::new(),
            microphone_test_active: false,
            provider_audio: None,
            provider_session_id: None,
            voice,
            recognition_context: String::new(),
            stream_failed: false,
        }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    pub fn capture_mut(&mut self) -> &mut C {
        &mut self.capture
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: EngineConfig) {
        self.config = config;
    }

    pub fn set_microphone_test_active(&mut self, active: bool) {
        self.microphone_test_active = active;
    }

    pub fn current_session(&self) -> Option<Arc<VoiceSession<T>>> {
        self.registry.current()
    }

    pub fn session_active(&self) -> bool {
        self.registry.has_active_session()
    }

    pub fn provider_alive(&self) -> bool {
        self.provider_session_id.is_some()
    }

    pub fn start(&mut self, request: StartRequest<T>) -> StartOutcome {
        let gate = start_gate(&StartConditions {
            microphone_test_active: self.microphone_test_active,
            processor_available: request.processor_available,
            update_installing: request.update_installing,
            transition_cancelled: request.transition_cancelled,
            session_active: self.registry.has_active_session(),
            previous_stream_alive: self.provider_session_id.is_some(),
            frontmost_settings: request.frontmost_settings,
            setup_trial_target: request.setup_trial_target,
            engine_label: &self.config.engine_label,
        });
        if let Err(reject) = gate {
            if reject.resets_keyboard() {
                if let Some(notice) = reject.notice() {
                    self.host.show_warning(&notice);
                    self.host.show_notice(&notice, NoticeScope::Idle);
                }
                self.host.reset_keyboard();
            }
            return StartOutcome::Rejected(reject);
        }

        self.stream_failed = false;
        self.recognition_context = request.recognition_context;
        self.voice = VoiceGate::new(
            self.config.voice_rms_threshold,
            self.config.minimum_total_ms,
            self.config.minimum_voiced_ms,
        );
        let target_locked = request.target.is_some();
        let session = Arc::new(VoiceSession::new(
            uuid::Uuid::new_v4().simple().to_string(),
            request.target,
            request.now,
        ));
        let session = match self.registry.try_begin(session, false) {
            Ok(session) => session,
            Err(BeginReject::AlreadyActive | BeginReject::PreviousStreamAlive) => {
                self.host.reset_keyboard();
                return StartOutcome::Rejected(StartReject::AlreadyActive);
            }
        };

        self.host.show_preparing();
        if let Err(error) = self.capture.start() {
            let reason = if error.trim().is_empty() {
                "麦克风启动失败，请检查设备与权限".to_string()
            } else {
                error
            };
            self.host
                .diagnostics(&map(&[("last_error", json!(reason))]));
            self.host.show_error(&reason);
            self.finish_session(&session, Some(&reason));
            return StartOutcome::CaptureFailed { reason };
        }

        if session.is_cancelled() || !self.registry.is_current(&session) {
            self.capture.stop(true);
            self.finish_session(&session, None);
            return StartOutcome::Cancelled;
        }

        self.host.mark_streaming();
        self.host.diagnostics(&map(&[
            (
                "microphone",
                json!(self.capture.current_device().unwrap_or_else(|| "未知".into())),
            ),
            ("sample_rate", json!(self.capture.sample_rate())),
            ("target_locked", json!(target_locked)),
            ("last_error", json!("")),
        ]));

        let (sender, audio) = mpsc::channel(PROVIDER_QUEUE_CHUNKS);
        self.provider_audio = Some((session.session_id.clone(), sender));
        self.provider_session_id = Some(session.session_id.clone());
        self.host.show_listening();
        self.host.update_preview(LISTENING_TEXT);
        StartOutcome::Started(ProviderStream {
            session_id: session.session_id.clone(),
            audio,
        })
    }

    pub fn audio_chunk(&mut self, chunk: &[u8]) {
        let Some(session) = self.registry.current() else {
            return;
        };
        if session.is_cancelled() {
            return;
        }
        let metrics = self.voice.push_pcm16_le(chunk);
        if metrics.samples > 0 {
            session.record_audio(metrics.total_audio_ms, metrics.voiced_audio_ms);
            self.host.update_level(metrics.level);
        }
        if let Some((_, sender)) = &self.provider_audio {
            // A stalled provider must not block the capture bridge; the oldest
            // 6.4 s of unacknowledged audio is the only thing at risk.
            let _ = sender.try_send(chunk.to_vec());
        }
    }

    pub fn provider_preview(&mut self, session_id: &str, text: &str, now: f64) {
        let Some(session) = self.matching_session(session_id) else {
            return;
        };
        if session.is_cancelled() {
            return;
        }
        if !self.voice.is_open() || is_context_echo(text, &self.recognition_context) {
            return;
        }
        let Some(preview) = self.prepare_text(text) else {
            return;
        };
        let elapsed_ms = ((now - session.started_at) * 1000.0) as i64;
        session.record_preview(preview.clone(), elapsed_ms);
        self.host.update_preview(&preview);
    }

    pub fn provider_final(&mut self, session_id: &str, text: &str) {
        if self.stream_failed || !self.capture.capture_error().is_empty() {
            return;
        }
        let Some(session) = self.matching_session(session_id) else {
            return;
        };
        if session.is_cancelled() {
            return;
        }
        let (total_audio_ms, voiced_audio_ms) = session.audio_ms();
        if !has_sufficient_speech_with(
            total_audio_ms as f64,
            voiced_audio_ms as f64,
            self.config.minimum_total_ms,
            self.config.minimum_voiced_ms,
        ) || is_context_echo(text, &self.recognition_context)
        {
            return;
        }
        let Some(text) = self.prepare_text(text) else {
            return;
        };
        if !session.begin_final() {
            return;
        }
        if self.config.archive_enabled {
            if let Some(path) = session.archive_path() {
                self.host.save_transcript(&path, &text);
            }
        }
        let target = session.output_target.clone();
        let committed = self.host.commit_text(&text, target.as_ref());
        session.set_final_committed(committed);
    }

    pub fn provider_complete(&mut self, session_id: &str) {
        let Some(session) = self.matching_session(session_id) else {
            return;
        };
        if session.is_cancelled() {
            return;
        }
        let capture_error = self.capture.capture_error();
        if !capture_error.is_empty() {
            self.fail_session(&session, &capture_error, "device_error");
            return;
        }

        let committed = session.final_committed();
        let had_preview = session.preview_count() > 0;
        let recovered = self.retain_session_preview(&session, "未收到完整识别结果");
        let mut outcome = if committed {
            "committed"
        } else if recovered {
            "recovered"
        } else if !had_preview {
            "no_safe_result"
        } else {
            "not_committed"
        };
        let mut notice = if committed {
            String::new()
        } else {
            "文字未写入，请打开恢复页查看".to_string()
        };
        if outcome == "no_safe_result" {
            let (total_audio_ms, voiced_audio_ms) = session.audio_ms();
            let empty = empty_result_notice(
                total_audio_ms as f64,
                voiced_audio_ms as f64,
                &self.config.hotkey_mode,
                &self.config.hotkey_label,
            );
            outcome = empty.outcome;
            notice = empty.notice;
        }
        if !committed {
            self.host.show_warning(&notice);
        }
        self.record_session_diagnostics(&session, outcome);
        self.finish_session(&session, Some(&notice));
    }

    pub fn provider_error(&mut self, session_id: &str, error: &str) {
        if self.stream_failed {
            return;
        }
        let Some(session) = self.matching_session(session_id) else {
            return;
        };
        self.stream_failed = true;
        let lowered = error.to_lowercase();
        let authentication = ["401", "403", "鉴权", "unauthorized", "invalid_api_key"]
            .iter()
            .any(|token| lowered.contains(token));
        let (reason, outcome) = if authentication {
            let reason = if self.config.service == "custom" {
                "API Key 未通过验证，请检查 Key 和模型权限"
            } else {
                "API Key 未通过验证，请在设置中检查 Key、地区和服务权限"
            };
            (reason.to_string(), "authentication_error")
        } else if self.config.service == "custom" || self.config.provider_is_builtin {
            (error.to_string(), "custom_provider_error")
        } else {
            (
                format!(
                    "{}连接中断，请检查网络后重试",
                    self.config.engine_label
                ),
                "network_error",
            )
        };
        self.fail_session(&session, &reason, outcome);
    }

    /// The provider future returned. A silent return is never success.
    pub fn provider_finished(&mut self, session_id: &str) {
        if self.provider_session_id.as_deref() == Some(session_id) {
            self.provider_session_id = None;
            self.provider_audio = None;
        }
        if let Some(session) = self.matching_session(session_id) {
            if !session.is_cancelled() {
                self.fail_session(&session, "识别未返回完整结果，请重试", "incomplete");
            }
        }
    }

    /// Forward captured chunks and capture lifecycle notices to the engine.
    /// The app pump calls this every few milliseconds while a session is live.
    pub fn poll_capture(&mut self) {
        for event in self.capture.poll_events() {
            match event {
                CaptureEvent::Chunk(chunk) => self.audio_chunk(&chunk),
                CaptureEvent::AutoStop => self.auto_stop(),
                CaptureEvent::DeviceError(_) => self.device_disconnected(),
            }
        }
    }

    pub fn stop(&mut self) {
        let Some(session) = self.registry.current() else {
            self.host.reset_keyboard();
            return;
        };
        if session.is_cancelled() {
            self.host.reset_keyboard();
            return;
        }
        self.host.show_processing();
        self.host.update_preview(PROCESSING_TEXT);
        let audio = self.capture.stop(false);
        // Flush the final partial block before closing the audio channel, or
        // the provider would finish without the post-roll tail.
        self.drain_capture_audio();
        self.provider_audio = None;
        self.archive_buffer(&session, audio);
    }

    pub fn cancel(&mut self) {
        let Some(session) = self.registry.current() else {
            self.capture.stop(true);
            return;
        };
        session.cancel();
        self.capture.stop(true);
        self.record_session_diagnostics(&session, "cancelled");
        self.finish_session(&session, None);
    }

    /// Python's `_recover_runtime`: keep whatever was recognised, then release
    /// the device so a restart can bind it again.
    pub fn recover_runtime(&mut self, reason: &str) {
        if let Some(session) = self.registry.current() {
            self.retain_session_preview(&session, reason);
            session.cancel();
            self.record_session_diagnostics(&session, "recovered");
            self.capture.stop(true);
            self.finish_session(&session, None);
        } else {
            self.capture.stop(true);
            self.host.reset_keyboard();
        }
        // A hard recovery abandons a provider stream that never finished.
        // Late callbacks carry the old session id and are ignored downstream.
        self.provider_session_id = None;
        self.provider_audio = None;
        self.capture.reset_streaming_state();
    }

    pub fn auto_stop(&mut self) {
        self.stop();
    }

    pub fn device_disconnected(&mut self) {
        self.host
            .diagnostics(&map(&[("last_error", json!("录音设备已断开"))]));
        self.stop();
    }

    fn matching_session(&self, session_id: &str) -> Option<Arc<VoiceSession<T>>> {
        let session = self.registry.current()?;
        (session.session_id.as_str() == session_id).then_some(session)
    }

    fn prepare_text(&self, text: &str) -> Option<String> {
        let formatted = format_transcription_text(text, self.config.punctuation.as_str()).ok()?;
        let cleaned = clean_spoken_disfluencies(
            &formatted,
            Some(self.config.disfluency_enabled),
            self.config.disfluency_mode.as_deref(),
        );
        let sanitized = sanitize_inline_text(&cleaned);
        let restored = match &self.config.vocabulary_spelling {
            Some(spelling) => spelling.restore(&sanitized),
            None => sanitized,
        };
        (!restored.is_empty()).then_some(restored)
    }

    fn drain_capture_audio(&mut self) {
        for event in self.capture.poll_events() {
            if let CaptureEvent::Chunk(chunk) = event {
                self.audio_chunk(&chunk);
            }
        }
    }

    fn archive_buffer(&mut self, session: &Arc<VoiceSession<T>>, audio: Option<Vec<u8>>) {
        let Some(audio) = audio.filter(|bytes| !bytes.is_empty()) else {
            return;
        };
        if !self.config.archive_enabled || session.is_cancelled() {
            return;
        }
        session.set_archive_path(self.host.archive_audio(&audio));
    }

    fn retain_session_preview(&mut self, session: &Arc<VoiceSession<T>>, reason: &str) -> bool {
        let Some(text) = session.claim_recovery() else {
            return false;
        };
        let reason = format!("{reason}{RECOVERY_HINT}");
        let saved = self.host.retain_recovery(&text, &reason);
        if !saved {
            session.release_recovery_claim();
        }
        saved
    }

    fn fail_session(&mut self, session: &Arc<VoiceSession<T>>, reason: &str, outcome: &str) {
        if !self.registry.is_current(session) || session.is_cancelled() {
            return;
        }
        let notice = if self.retain_session_preview(session, reason) {
            RECOVERY_NOTICE.to_string()
        } else {
            self.host.show_error(reason);
            reason.to_string()
        };
        self.host
            .diagnostics(&map(&[("last_error", json!(reason))]));
        self.record_session_diagnostics(session, outcome);
        self.finish_session(session, Some(&notice));
    }

    fn record_session_diagnostics(&mut self, session: &Arc<VoiceSession<T>>, outcome: &str) {
        session.set_outcome(outcome);
        self.host.diagnostics(&session.diagnostics());
    }

    fn finish_session(&mut self, session: &Arc<VoiceSession<T>>, notice: Option<&str>) {
        if !self.registry.finish(session) {
            return;
        }
        self.capture.reset_streaming_state();
        self.provider_audio = None;
        self.host.hide_preview();
        self.host.reset_keyboard();
        if let Some(notice) = notice.filter(|text| !text.is_empty()) {
            self.host
                .show_notice(notice, NoticeScope::FinishedSession(session.session_id.clone()));
        }
    }
}

fn map(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct HostLog {
        events: Vec<String>,
        committed: Vec<(String, Option<u32>)>,
        recoveries: Vec<(String, String)>,
        diagnostics: Vec<Map<String, Value>>,
        archived: Vec<Vec<u8>>,
        transcripts: Vec<(PathBuf, String)>,
        commit_result: bool,
        retain_result: bool,
    }

    impl HostLog {
        fn saw(&self, event: &str) -> bool {
            self.events.iter().any(|item| item == event)
        }

        fn diagnostics_value(&self, key: &str) -> Option<Value> {
            self.diagnostics
                .iter()
                .rev()
                .find_map(|values| values.get(key).cloned())
        }
    }

    impl EngineHost<u32> for HostLog {
        fn show_preparing(&mut self) {
            self.events.push("preparing".into());
        }
        fn show_listening(&mut self) {
            self.events.push("listening".into());
        }
        fn show_processing(&mut self) {
            self.events.push("processing".into());
        }
        fn update_preview(&mut self, text: &str) {
            self.events.push(format!("preview:{text}"));
        }
        fn update_level(&mut self, level: f64) {
            self.events.push(format!("level:{level:.3}"));
        }
        fn hide_preview(&mut self) {
            self.events.push("hide".into());
        }
        fn show_notice(&mut self, notice: &str, _scope: NoticeScope) {
            self.events.push(format!("notice:{notice}"));
        }
        fn show_warning(&mut self, notice: &str) {
            self.events.push(format!("warning:{notice}"));
        }
        fn show_error(&mut self, message: &str) {
            self.events.push(format!("error:{message}"));
        }
        fn mark_streaming(&mut self) {
            self.events.push("mark_streaming".into());
        }
        fn reset_keyboard(&mut self) {
            self.events.push("reset_keyboard".into());
        }
        fn commit_text(&mut self, text: &str, target: Option<&u32>) -> bool {
            self.committed.push((text.to_string(), target.copied()));
            self.commit_result
        }
        fn retain_recovery(&mut self, text: &str, reason: &str) -> bool {
            self.recoveries.push((text.to_string(), reason.to_string()));
            self.retain_result
        }
        fn diagnostics(&mut self, values: &Map<String, Value>) {
            self.diagnostics.push(values.clone());
        }
        fn archive_audio(&mut self, audio: &[u8]) -> Option<PathBuf> {
            self.archived.push(audio.to_vec());
            Some(PathBuf::from("/tmp/archive.wav"))
        }
        fn save_transcript(&mut self, archive_path: &Path, text: &str) {
            self.transcripts
                .push((archive_path.to_path_buf(), text.to_string()));
        }
    }

    struct FakeCapture {
        start_error: Option<String>,
        audio: Option<Vec<u8>>,
        error: String,
        device: Option<String>,
        stops: Vec<bool>,
        resets: u32,
        events: Vec<CaptureEvent>,
    }

    impl Default for FakeCapture {
        fn default() -> Self {
            Self {
                start_error: None,
                audio: None,
                error: String::new(),
                device: Some("Test Mic".into()),
                stops: Vec::new(),
                resets: 0,
                events: Vec::new(),
            }
        }
    }

    impl AudioCapture for FakeCapture {
        fn start(&mut self) -> Result<(), String> {
            match &self.start_error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
        fn stop(&mut self, abort: bool) -> Option<Vec<u8>> {
            self.stops.push(abort);
            self.audio.take()
        }
        fn reset_streaming_state(&mut self) {
            self.resets += 1;
        }
        fn current_device(&self) -> Option<String> {
            self.device.clone()
        }
        fn sample_rate(&self) -> Option<u32> {
            Some(16_000)
        }
        fn capture_error(&self) -> String {
            self.error.clone()
        }
        fn poll_events(&mut self) -> Vec<CaptureEvent> {
            std::mem::take(&mut self.events)
        }
    }

    type Engine = VoiceEngine<u32, HostLog, FakeCapture>;

    fn engine() -> Engine {
        VoiceEngine::new(EngineConfig::default(), HostLog::default(), FakeCapture::default())
    }

    fn start_request(now: f64, target: Option<u32>) -> StartRequest<u32> {
        StartRequest {
            now,
            target,
            processor_available: true,
            recognition_context: String::new(),
            frontmost_settings: false,
            setup_trial_target: false,
            transition_cancelled: false,
            update_installing: false,
        }
    }

    fn started(engine: &mut Engine, now: f64, target: Option<u32>) -> ProviderStream {
        match engine.start(start_request(now, target)) {
            StartOutcome::Started(stream) => stream,
            other => panic!("expected start, got {other:?}"),
        }
    }

    fn loud_chunk() -> Vec<u8> {
        1000i16.to_le_bytes().repeat(1600)
    }

    fn silent_chunk() -> Vec<u8> {
        0i16.to_le_bytes().repeat(1600)
    }

    fn rejected(outcome: StartOutcome) -> StartReject {
        match outcome {
            StartOutcome::Rejected(reject) => reject,
            other => panic!("expected rejection, got {other:?}"),
        }
    }

    #[test]
    fn start_orders_ui_capture_and_diagnostics() {
        let mut engine = engine();
        let stream = started(&mut engine, 100.0, Some(7));
        let events = &engine.host().events;
        assert_eq!(
            events,
            &[
                "preparing",
                "mark_streaming",
                "listening",
                "preview:正在听写…"
            ]
        );
        assert_eq!(
            engine.host().diagnostics_value("microphone"),
            Some(json!("Test Mic"))
        );
        assert_eq!(
            engine.host().diagnostics_value("target_locked"),
            Some(json!(true))
        );
        assert_eq!(engine.current_session().unwrap().session_id, stream.session_id);
        assert!(engine.provider_alive());
    }

    #[test]
    fn capture_failure_is_reported_and_resets_the_session() {
        let mut engine = engine();
        engine.capture_mut().start_error = Some("no input device".into());
        let outcome = engine.start(start_request(0.0, None));
        match outcome {
            StartOutcome::CaptureFailed { reason } => assert_eq!(reason, "no input device"),
            other => panic!("expected capture failure, got {other:?}"),
        }
        assert!(engine.host().saw("error:no input device"));
        assert!(engine.host().saw("notice:no input device"));
        assert!(engine.host().saw("hide"));
        assert!(engine.host().saw("reset_keyboard"));
        assert!(!engine.session_active());
        assert!(!engine.provider_alive());
    }

    #[test]
    fn empty_capture_error_uses_the_python_fallback() {
        let mut engine = engine();
        engine.capture_mut().start_error = Some(String::new());
        let outcome = engine.start(start_request(0.0, None));
        match outcome {
            StartOutcome::CaptureFailed { reason } => {
                assert_eq!(reason, "麦克风启动失败，请检查设备与权限")
            }
            other => panic!("expected capture failure, got {other:?}"),
        }
    }

    #[test]
    fn gates_show_the_feedback_and_release_the_key() {
        let mut engine = engine();
        engine.set_microphone_test_active(true);
        let outcome = engine.start(start_request(0.0, None));
        assert_eq!(rejected(outcome), StartReject::MicrophoneTestActive);
        assert!(engine.host().saw("warning:麦克风正在测试，请稍候再开始听写"));
        assert!(engine.host().saw("notice:麦克风正在测试，请稍候再开始听写"));
        assert!(engine.host().saw("reset_keyboard"));
        engine.set_microphone_test_active(false);

        let mut unavailable = start_request(0.0, None);
        unavailable.processor_available = false;
        assert_eq!(
            rejected(engine.start(unavailable)),
            StartReject::Unconfigured {
                engine_label: "千问".into()
            }
        );

        let mut settings = start_request(0.0, None);
        settings.frontmost_settings = true;
        assert_eq!(
            rejected(engine.start(settings)),
            StartReject::SettingsFocused
        );
        let mut trial = start_request(0.0, None);
        trial.frontmost_settings = true;
        trial.setup_trial_target = true;
        assert!(matches!(
            engine.start(trial),
            StartOutcome::Started(_)
        ));
    }

    #[test]
    fn cancelled_transition_is_silent() {
        let mut engine = engine();
        let mut request = start_request(0.0, None);
        request.transition_cancelled = true;
        assert_eq!(
            rejected(engine.start(request)),
            StartReject::TransitionCancelled
        );
        assert!(engine.host().events.is_empty());
    }

    #[test]
    fn previews_wait_for_the_voice_gate() {
        let mut engine = engine();
        let stream = started(&mut engine, 100.0, None);
        engine.provider_preview(&stream.session_id, "太早了", 100.5);
        assert!(!engine
            .host()
            .events
            .iter()
            .any(|event| event == "preview:太早了"));

        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine.provider_preview(&stream.session_id, "你好", 100.5);
        assert!(engine
            .host()
            .events
            .iter()
            .any(|event| event == "preview:你好"));
        let session = engine.current_session().unwrap();
        assert_eq!(session.latest_preview(), "你好");
        assert_eq!(session.preview_count(), 1);
        assert_eq!(session.first_preview_ms(), Some(500));
    }

    #[test]
    fn preview_echo_is_dropped() {
        let mut engine = engine();
        let mut request = start_request(0.0, None);
        request.recognition_context =
            "数据工作 平台 负责离线特征和实时特征开发以及数据质量监控和模型评估".into();
        let StartOutcome::Started(stream) = engine.start(request) else {
            panic!("expected start");
        };
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine.provider_preview(
            &stream.session_id,
            "数据工作平台负责离线特征和实时特征开发以及数据质量监控和模型评估",
            0.5,
        );
        let session = engine.current_session().unwrap();
        assert_eq!(session.preview_count(), 0);
        assert_eq!(session.latest_preview(), "");
    }

    #[test]
    fn final_text_is_prepared_and_committed_once() {
        let mut engine = engine();
        engine.host_mut().commit_result = true;
        let stream = started(&mut engine, 0.0, Some(42));
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine.provider_final(&stream.session_id, "嗯 你好。");
        engine.provider_final(&stream.session_id, "重复结果");

        assert_eq!(
            engine.host().committed,
            vec![("嗯 你好。".to_string(), Some(42))]
        );
        assert!(engine.current_session().unwrap().final_committed());

        engine.provider_complete(&stream.session_id);
        assert_eq!(
            engine.host().diagnostics_value("last_session_outcome"),
            Some(json!("committed"))
        );
        assert!(!engine.session_active());
        assert!(engine.host().saw("hide"));
        assert!(!engine
            .host()
            .events
            .iter()
            .any(|event| event.starts_with("notice:")));
    }

    #[test]
    fn disfluency_and_punctuation_settings_shape_the_committed_text() {
        let config = EngineConfig {
            punctuation: PunctuationMode::None,
            disfluency_enabled: true,
            disfluency_mode: Some("conservative".into()),
            ..EngineConfig::default()
        };
        let mut engine = VoiceEngine::new(config, HostLog::default(), FakeCapture::default());
        engine.host_mut().commit_result = true;
        let stream = started(&mut engine, 0.0, None);
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine.provider_final(&stream.session_id, "嗯，你好。");
        assert_eq!(engine.host().committed[0].0, "你好");
    }

    #[test]
    fn hotword_spelling_is_restored_before_commit() {
        let restorer = crate::custom_vocabulary::spelling_restorer(&["user_id".into()])
            .map(Arc::new);
        let config = EngineConfig {
            punctuation: PunctuationMode::Spaces,
            vocabulary_spelling: restorer,
            ..EngineConfig::default()
        };
        let mut engine = VoiceEngine::new(config, HostLog::default(), FakeCapture::default());
        engine.host_mut().commit_result = true;
        let stream = started(&mut engine, 0.0, None);
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine.provider_final(&stream.session_id, "user id");
        assert_eq!(engine.host().committed[0].0, "user_id");
    }

    #[test]
    fn short_or_silent_results_are_never_committed() {
        let mut short_engine = engine();
        short_engine.host_mut().commit_result = true;
        let short = started(&mut short_engine, 0.0, None);
        short_engine.provider_final(&short.session_id, "你好");
        assert!(short_engine.host().committed.is_empty());
        assert!(!short_engine
            .current_session()
            .unwrap()
            .final_emitted());
        short_engine.provider_complete(&short.session_id);
        assert_eq!(
            short_engine
                .host()
                .diagnostics_value("last_session_outcome"),
            Some(json!("too_short"))
        );
        assert!(short_engine
            .host()
            .events
            .iter()
            .any(|event| event == "warning:录音过短，请按住右 Option说完再松开"));

        // A completed provider with only silence reports silence, not a mic error.
        let mut silent_engine = engine();
        let silent = started(&mut silent_engine, 0.0, None);
        for _ in 0..6 {
            silent_engine.audio_chunk(&silent_chunk());
        }
        silent_engine.provider_final(&silent.session_id, "幻听");
        silent_engine.provider_complete(&silent.session_id);
        assert_eq!(
            silent_engine
                .host()
                .diagnostics_value("last_session_outcome"),
            Some(json!("silence"))
        );
    }

    #[test]
    fn completion_without_a_final_keeps_the_preview_for_recovery() {
        let mut engine = engine();
        engine.host_mut().retain_result = true;
        let stream = started(&mut engine, 0.0, None);
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine.provider_preview(&stream.session_id, "没有最终结果", 0.4);
        engine.provider_complete(&stream.session_id);

        assert_eq!(
            engine.host().recoveries,
            vec![(
                "没有最终结果".to_string(),
                "未收到完整识别结果；已识别片段可在恢复页复制".to_string()
            )]
        );
        assert_eq!(
            engine.host().diagnostics_value("last_session_outcome"),
            Some(json!("recovered"))
        );
        // Python keeps the generic completion notice even after saving the
        // preview; the recovery wording is only a hint on the entry itself.
        assert!(engine
            .host()
            .events
            .iter()
            .any(|event| event == "notice:文字未写入，请打开恢复页查看"));
    }

    #[test]
    fn not_committed_outcome_keeps_the_generic_notice() {
        let mut engine = engine();
        let stream = started(&mut engine, 0.0, None);
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine.provider_preview(&stream.session_id, "有预览", 0.4);
        engine.provider_complete(&stream.session_id);
        assert_eq!(
            engine.host().diagnostics_value("last_session_outcome"),
            Some(json!("not_committed"))
        );
        assert!(engine
            .host()
            .events
            .iter()
            .any(|event| event == "warning:文字未写入，请打开恢复页查看"));
    }

    #[test]
    fn cancel_blocks_late_callbacks_and_never_recovers() {
        let mut engine = engine();
        engine.host_mut().commit_result = true;
        engine.host_mut().retain_result = true;
        let stream = started(&mut engine, 0.0, None);
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine.provider_preview(&stream.session_id, "取消掉", 0.4);
        engine.cancel();

        assert_eq!(engine.capture_mut().stops, vec![true]);
        assert!(!engine.session_active());
        assert_eq!(
            engine.host().diagnostics_value("last_session_outcome"),
            Some(json!("cancelled"))
        );
        assert!(engine.host().recoveries.is_empty());

        engine.provider_preview(&stream.session_id, "迟到预览", 0.6);
        engine.provider_final(&stream.session_id, "迟到结果");
        engine.provider_complete(&stream.session_id);
        assert!(engine.host().committed.is_empty());
        assert!(engine.host().recoveries.is_empty());
        assert!(engine.host().saw("hide"));
    }

    #[test]
    fn provider_error_maps_authentication_for_qwen_and_custom() {
        let mut engine = engine();
        let stream = started(&mut engine, 0.0, None);
        engine.provider_error(&stream.session_id, "HTTP 401 Unauthorized");
        assert_eq!(
            engine.host().diagnostics_value("last_session_outcome"),
            Some(json!("authentication_error"))
        );
        assert!(engine.host().events.iter().any(|event| event
            == "error:API Key 未通过验证，请在设置中检查 Key、地区和服务权限"));

        let config = EngineConfig {
            service: "custom".into(),
            ..EngineConfig::default()
        };
        let mut custom = VoiceEngine::new(config, HostLog::default(), FakeCapture::default());
        let stream = started(&mut custom, 0.0, None);
        custom.provider_error(&stream.session_id, "invalid_api_key");
        assert!(custom
            .host()
            .events
            .iter()
            .any(|event| event == "error:API Key 未通过验证，请检查 Key 和模型权限"));
    }

    #[test]
    fn network_errors_retain_the_preview_when_available() {
        let mut engine = engine();
        engine.host_mut().retain_result = true;
        let stream = started(&mut engine, 0.0, None);
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine.provider_preview(&stream.session_id, "网络断了", 0.4);
        engine.provider_error(&stream.session_id, "websocket closed");

        assert_eq!(
            engine.host().diagnostics_value("last_session_outcome"),
            Some(json!("network_error"))
        );
        assert_eq!(
            engine.host().diagnostics_value("last_error"),
            Some(json!("千问连接中断，请检查网络后重试"))
        );
        assert_eq!(engine.host().recoveries.len(), 1);
        assert!(!engine
            .host()
            .events
            .iter()
            .any(|event| event.starts_with("error:")));
    }

    #[test]
    fn silent_provider_return_is_incomplete_and_late_duplicates_are_ignored() {
        let mut engine = engine();
        let stream = started(&mut engine, 0.0, None);
        engine.provider_finished(&stream.session_id);
        assert_eq!(
            engine.host().diagnostics_value("last_session_outcome"),
            Some(json!("incomplete"))
        );
        assert!(engine.host().events.iter().any(|event| event
            == "error:识别未返回完整结果，请重试"));
        let events = engine.host().events.len();
        engine.provider_finished(&stream.session_id);
        assert_eq!(engine.host().events.len(), events);
    }

    #[test]
    fn a_live_provider_blocks_a_new_session_until_it_returns() {
        let mut engine = engine();
        let stream = started(&mut engine, 0.0, None);
        engine.cancel();
        assert!(engine.provider_alive());
        assert_eq!(
            rejected(engine.start(start_request(1.0, None))),
            StartReject::PreviousStreamAlive
        );
        assert!(engine.host().saw("reset_keyboard"));

        engine.provider_finished(&stream.session_id);
        assert!(!engine.provider_alive());
        assert!(matches!(
            engine.start(start_request(2.0, None)),
            StartOutcome::Started(_)
        ));
    }

    #[test]
    fn stop_shows_processing_stops_capture_and_archives() {
        let config = EngineConfig {
            archive_enabled: true,
            ..EngineConfig::default()
        };
        let capture = FakeCapture {
            audio: Some(vec![1, 2, 3]),
            ..FakeCapture::default()
        };
        let mut engine = VoiceEngine::new(config, HostLog::default(), capture);
        let stream = started(&mut engine, 0.0, None);
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine.stop();

        assert!(engine.host().saw("processing"));
        assert!(engine.host().saw("preview:正在完成识别…"));
        assert_eq!(engine.capture_mut().stops, vec![false]);
        assert_eq!(engine.host().archived, vec![vec![1, 2, 3]]);
        assert_eq!(
            engine.current_session().unwrap().archive_path(),
            Some(PathBuf::from("/tmp/archive.wav"))
        );
        // The session remains current until the provider completes.
        assert!(engine.session_active());

        engine.host_mut().commit_result = true;
        engine.provider_final(&stream.session_id, "存档结果");
        assert_eq!(
            engine.host().transcripts,
            vec![(PathBuf::from("/tmp/archive.wav"), "存档结果".into())]
        );
    }

    #[test]
    fn recovery_from_a_stuck_processing_resets_the_keyboard() {
        let mut engine = engine();
        let _stream = started(&mut engine, 0.0, None);
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine.stop();
        assert!(engine.host().saw("processing"));
        assert!(engine.session_active());

        engine.recover_runtime("识别结束超时");

        assert!(engine.host().saw("reset_keyboard"));
        assert!(!engine.session_active());
        assert!(!engine.provider_alive());
        assert_eq!(
            engine.host().diagnostics_value("last_session_outcome"),
            Some(json!("recovered"))
        );
    }

    #[test]
    fn device_disconnect_records_the_reason_then_stops() {
        let mut engine = engine();
        let _stream = started(&mut engine, 0.0, None);
        engine.device_disconnected();
        assert_eq!(
            engine.host().diagnostics_value("last_error"),
            Some(json!("录音设备已断开"))
        );
        assert!(engine.host().saw("processing"));
        assert_eq!(engine.capture_mut().stops, vec![false]);
    }

    #[test]
    fn capture_error_after_completion_is_a_device_failure() {
        let mut engine = engine();
        let stream = started(&mut engine, 0.0, None);
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine.capture_mut().error = "core audio device lost".into();
        engine.provider_complete(&stream.session_id);
        assert_eq!(
            engine.host().diagnostics_value("last_session_outcome"),
            Some(json!("device_error"))
        );
        assert!(engine.host().events.iter().any(|event| event
            == "error:core audio device lost"));
    }

    #[test]
    fn cancel_without_a_session_still_aborts_capture() {
        let mut engine = engine();
        engine.cancel();
        assert_eq!(engine.capture_mut().stops, vec![true]);
    }

    #[test]
    fn audio_metrics_and_levels_follow_the_gate() {
        let mut engine = engine();
        let _stream = started(&mut engine, 0.0, None);
        engine.audio_chunk(&loud_chunk());
        let (total, voiced) = engine.current_session().unwrap().audio_ms();
        assert_eq!((total, voiced), (100, 100));
        assert!(engine
            .host()
            .events
            .iter()
            .any(|event| event.starts_with("level:")));
    }

    #[test]
    fn stop_flushes_the_capture_tail_then_closes_the_provider_audio() {
        let mut engine = engine();
        let mut stream = started(&mut engine, 0.0, None);
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        let tail = vec![9u8, 8, 7, 6];
        engine
            .capture_mut()
            .events
            .push(CaptureEvent::Chunk(tail.clone()));
        engine.stop();

        let mut received = Vec::new();
        while let Ok(chunk) = stream.audio.try_recv() {
            received.push(chunk);
        }
        assert_eq!(received.len(), 3);
        assert_eq!(received.last().unwrap(), &tail);
        assert!(stream.audio.try_recv().is_err());
        // The provider future has not returned yet, so a fast second press
        // must still be rejected.
        assert!(engine.provider_alive());
        assert_eq!(engine.capture_mut().stops, vec![false]);
    }

    #[test]
    fn auto_stop_event_submits_the_captured_audio() {
        let mut engine = engine();
        engine.capture_mut().audio = Some(b"wav".to_vec());
        let _stream = started(&mut engine, 0.0, None);
        engine
            .capture_mut()
            .events
            .push(CaptureEvent::AutoStop);
        engine.poll_capture();
        assert!(engine.host().saw("processing"));
        assert!(engine.session_active());
        assert_eq!(engine.capture_mut().stops, vec![false]);
        assert!(engine.host().archived.is_empty());
    }

    #[test]
    fn device_error_event_becomes_a_device_failure_at_completion() {
        let mut engine = engine();
        engine.capture_mut().error = "core audio device lost".into();
        let stream = started(&mut engine, 0.0, None);
        engine.audio_chunk(&loud_chunk());
        engine.audio_chunk(&loud_chunk());
        engine
            .capture_mut()
            .events
            .push(CaptureEvent::DeviceError("device lost".into()));
        engine.poll_capture();
        assert_eq!(
            engine.host().diagnostics_value("last_error"),
            Some(json!("录音设备已断开"))
        );
        assert!(engine.host().saw("processing"));

        engine.provider_complete(&stream.session_id);
        assert_eq!(
            engine.host().diagnostics_value("last_session_outcome"),
            Some(json!("device_error"))
        );
    }
}
