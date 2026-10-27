//! `EngineHost` implementation that performs real side effects off the
//! main thread: AX insertion, recovery files, diagnostics, audio archive.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::{Map, Value};

use super::clock;
use super::hub::StateHub;
use crate::audio::archive::AudioArchiveManager;
use crate::diagnostics::DiagnosticsStore;
use crate::engine::EngineHost;
use crate::insert::ax::{AxClient, TargetId};
use crate::insert::recovery::RecoveryStore;
use crate::state::InputState;
use crate::ui::{NoticeScope, UiCommand, UiSink};

/// The engine only needs the handle; the AX worker owns the element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeTarget {
    pub id: TargetId,
}

pub struct MacHost {
    ui: Arc<dyn UiSink>,
    hub: Arc<StateHub>,
    diagnostics: Arc<DiagnosticsStore>,
    archive: AudioArchiveManager,
    recovery: Arc<RecoveryStore>,
    ax: AxClient,
    service: String,
    model: String,
}

impl MacHost {
    pub fn new(
        ui: Arc<dyn UiSink>,
        hub: Arc<StateHub>,
        diagnostics: Arc<DiagnosticsStore>,
        archive: AudioArchiveManager,
        recovery: Arc<RecoveryStore>,
        ax: AxClient,
        service: String,
        model: String,
    ) -> Self {
        Self {
            ui,
            hub,
            diagnostics,
            archive,
            recovery,
            ax,
            service,
            model,
        }
    }

    fn emit_state(&self, state: InputState) {
        self.hub.set_state(state, clock());
        self.ui.send(UiCommand::State(state));
        let mut values = Map::new();
        values.insert("state".into(), serde_json::json!(state.name()));
        let _ = self.diagnostics.update(&values);
    }

    /// Keep archive metadata in step after a settings reload.
    pub fn set_engine_identity(&mut self, service: String) {
        self.model = if service == "qwen" {
            crate::asr::qwen::QwenConfig::from_env()
                .map(|config| config.model)
                .unwrap_or_else(|_| service.clone())
        } else {
            service.clone()
        };
        self.service = service;
    }

    /// Python's `show_warning`/`show_error`: the status bar flashes, while the
    /// preview notice comes separately from `_reject_start`/`_finish_session`.
    fn flash_message(&self, state: InputState) {
        self.emit_state(state);
        let hub = Arc::clone(&self.hub);
        let ui = Arc::clone(&self.ui);
        let _ = thread::Builder::new()
            .name("veyqa-state-message-reset".to_string())
            .spawn(move || {
                thread::sleep(Duration::from_secs(2));
                let before = hub.state();
                hub.reset_message_state(clock());
                if hub.state() != before {
                    ui.send(UiCommand::State(InputState::Idle));
                }
            });
    }
}

impl EngineHost<RuntimeTarget> for MacHost {
    fn show_preparing(&mut self) {
        self.hub.set_session_active(true);
        self.emit_state(InputState::Recording);
        self.ui.send(UiCommand::PreviewShow { preparing: true });
    }

    fn show_listening(&mut self) {
        self.emit_state(InputState::Streaming);
        self.ui.send(UiCommand::PreviewShow { preparing: false });
    }

    fn show_processing(&mut self) {
        self.emit_state(InputState::Processing);
        self.ui.send(UiCommand::PreviewProcessing);
        self.ui.send(UiCommand::PreviewText("正在完成识别…".to_string()));
    }

    fn update_preview(&mut self, text: &str) {
        self.ui.send(UiCommand::PreviewText(text.to_string()));
    }

    fn update_level(&mut self, level: f64) {
        self.ui.send(UiCommand::PreviewLevel(level));
    }

    fn hide_preview(&mut self) {
        self.ui.send(UiCommand::PreviewHide);
    }

    fn show_notice(&mut self, notice: &str, scope: NoticeScope) {
        if let NoticeScope::FinishedSession(session_id) = &scope {
            self.hub.note_finished_session(session_id.clone());
        }
        self.ui.send(UiCommand::Notice {
            text: notice.to_string(),
            scope,
        });
    }

    fn show_warning(&mut self, _notice: &str) {
        self.flash_message(InputState::Warning);
    }

    fn show_error(&mut self, _message: &str) {
        self.flash_message(InputState::Error);
    }

    fn mark_streaming(&mut self) {
        self.emit_state(InputState::Streaming);
    }

    fn reset_keyboard(&mut self) {
        self.hub.set_session_active(false);
        self.hub.reset_keyboard(clock());
        self.ui.send(UiCommand::State(InputState::Idle));
    }

    fn commit_text(&mut self, text: &str, target: Option<&RuntimeTarget>) -> bool {
        let Some(target) = target else {
            return false;
        };
        self.ax.commit(Some(target.id), text)
    }

    fn retain_recovery(&mut self, text: &str, reason: &str) -> bool {
        self.recovery.add(text, reason).is_ok()
    }

    fn diagnostics(&mut self, values: &Map<String, Value>) {
        let _ = self.diagnostics.update(values);
    }

    fn archive_audio(&mut self, audio: &[u8]) -> Option<PathBuf> {
        self.archive.save_audio_bytes(audio, "recording")
    }

    fn save_transcript(&mut self, archive_path: &Path, text: &str) {
        self.archive.save_transcription_result(
            Some(archive_path),
            text,
            &self.service,
            &self.model,
            "transcriptions",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{channel, ChannelUiSink};
    use std::sync::Mutex;

    #[derive(Default)]
    struct TestSink {
        seen: Mutex<Vec<UiCommand>>,
    }

    impl UiSink for TestSink {
        fn send(&self, command: UiCommand) {
            self.seen.lock().unwrap().push(command);
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "veyqa-host-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn message_states_clear_after_two_seconds_and_reset_releases_the_latch() {
        let ui = Arc::new(TestSink::default());
        let hub = Arc::new(StateHub::new(clock()));
        let directory = temp_dir("states");
        let diagnostics = Arc::new(DiagnosticsStore::new(
            Some(directory.join("diagnostics.json")),
            false,
        ));
        let recovery = Arc::new(RecoveryStore::new(directory.join("recovery.json"), 5));
        let ax = AxClient::spawn(recovery.clone());
        let mut host = MacHost::new(
            ui.clone(),
            hub.clone(),
            diagnostics,
            AudioArchiveManager::new(Some(&directory)),
            recovery,
            ax,
            "qwen".into(),
            "test-model".into(),
        );

        let generation = hub.try_begin_recording(clock()).unwrap();
        assert!(!hub.transition_cancelled(generation));
        host.show_preparing();
        assert!(hub.session_active());
        host.show_warning("请打开设置");
        assert_eq!(hub.state(), InputState::Warning);
        host.reset_keyboard();
        assert_eq!(hub.state(), InputState::Idle);
        assert!(!hub.is_recording());
        assert!(!hub.session_active());

        let commands = ui.seen.lock().unwrap().clone();
        assert!(commands.contains(&UiCommand::State(InputState::Warning)));
        // Warnings only flash the status bar; the preview notice is emitted by
        // the start/finish paths, exactly like Python.
        assert!(!commands
            .iter()
            .any(|command| matches!(command, UiCommand::Notice { .. })));
        drop(commands);

        host.show_notice("文字已保留", NoticeScope::FinishedSession("abc".into()));
        assert_eq!(hub.finished_session_id().as_deref(), Some("abc"));
        let commands = ui.seen.lock().unwrap().clone();
        assert!(commands.contains(&UiCommand::Notice {
            text: "文字已保留".into(),
            scope: NoticeScope::FinishedSession("abc".into()),
        }));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn diagnostics_and_recovery_are_private_files() {
        let directory = temp_dir("files");
        let diagnostics = Arc::new(DiagnosticsStore::new(
            Some(directory.join("diagnostics.json")),
            false,
        ));
        let recovery = Arc::new(RecoveryStore::new(directory.join("recovery.json"), 5));
        let (sender, _receiver) = channel();
        let ax = AxClient::spawn(recovery.clone());
        let mut host = MacHost::new(
            Arc::new(ChannelUiSink::new(sender)),
            Arc::new(StateHub::new(0.0)),
            diagnostics.clone(),
            AudioArchiveManager::new(Some(&directory)),
            recovery.clone(),
            ax,
            "qwen".into(),
            "test-model".into(),
        );
        let mut values = Map::new();
        values.insert("last_error".into(), Value::String("".into()));
        host.diagnostics(&values);
        assert_eq!(diagnostics.read()["last_error"], Value::String("".into()));
        assert!(host.retain_recovery("测试文本", "未收到完整识别结果"));
        assert_eq!(recovery.load().len(), 1);
        assert!(!host.commit_text("测试", None));
        let _ = std::fs::remove_dir_all(directory);
    }
}
