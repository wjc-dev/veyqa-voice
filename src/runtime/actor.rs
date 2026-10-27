//! The single thread that owns `VoiceEngine`.
//!
//! All session transitions, capture polling, provider callbacks, and settings
//! reloads are serialized here, so no lock can interleave two utterances.

use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{select, Receiver, Sender};

use super::clock;
use super::host::{MacHost, RuntimeTarget};
use super::hub::StateHub;
use super::metrics::{self, SessionMetrics};
use super::sink::TransitionSink;
use super::EngineCommand;
use crate::asr::custom::CustomAsrSettings;
use crate::asr::custom_provider::run_custom;
use crate::asr::doubao::request_settings_from_environment;
use crate::asr::doubao_provider::{run_doubao, DoubaoConfig};
use crate::asr::qwen::QwenConfig;
use crate::asr::qwen_provider::run_qwen;
use crate::audio::archive::AudioArchiveManager;
use crate::audio::capture::CpalCapture;
use crate::diagnostics::DiagnosticsStore;
use crate::engine::{
    ProviderEvent, ProviderStream, StartOutcome, StartRequest, VoiceEngine,
};
use crate::insert::ax::{self, AxClient};
use crate::insert::corrections::CorrectionStore;
use crate::insert::recovery::RecoveryStore;
use crate::settings::RuntimeSettings;
use crate::ui::UiSink;

const POLL_ACTIVE: Duration = Duration::from_millis(5);
const POLL_IDLE: Duration = Duration::from_millis(100);

pub struct EngineActorParts {
    pub settings: RuntimeSettings,
    pub hub: Arc<StateHub>,
    pub ui: Arc<dyn UiSink>,
    pub diagnostics: Arc<DiagnosticsStore>,
    pub recovery: Arc<RecoveryStore>,
    pub corrections: Arc<CorrectionStore>,
    pub archive: AudioArchiveManager,
    pub ax: AxClient,
    pub capture: CpalCapture,
    pub tokio: tokio::runtime::Handle,
}

pub struct EngineActorHandle {
    commands: Sender<EngineCommand>,
    thread: Option<JoinHandle<()>>,
}

impl EngineActorHandle {
    pub fn spawn(parts: EngineActorParts) -> Self {
        let (commands, receiver) = crossbeam_channel::bounded(64);
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let actor = EngineActor::new(parts, receiver, events_rx, events_tx);
        let thread = thread::Builder::new()
            .name("veyqa-engine".to_string())
            .spawn(move || actor.run())
            .expect("无法启动语音引擎线程");
        Self {
            commands,
            thread: Some(thread),
        }
    }

    pub fn sender(&self) -> Sender<EngineCommand> {
        self.commands.clone()
    }

    pub fn sink(&self, hub: Arc<StateHub>) -> TransitionSink {
        TransitionSink::new(hub, self.commands.clone())
    }

    pub fn shutdown(&mut self) {
        let _ = self.commands.send(EngineCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for EngineActorHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct EngineActor {
    engine: VoiceEngine<RuntimeTarget, MacHost, CpalCapture>,
    settings: RuntimeSettings,
    hub: Arc<StateHub>,
    corrections: Arc<CorrectionStore>,
    ax: AxClient,
    commands: Receiver<EngineCommand>,
    provider_events: Receiver<ProviderEvent>,
    provider_tx: Sender<ProviderEvent>,
    tokio: tokio::runtime::Handle,
    metrics: Option<SessionMetrics>,
    metrics_failed: bool,
    metrics_cancelled: bool,
}

impl EngineActor {
    fn new(
        parts: EngineActorParts,
        commands: Receiver<EngineCommand>,
        provider_events: Receiver<ProviderEvent>,
        provider_tx: Sender<ProviderEvent>,
    ) -> Self {
        let model = if parts.settings.service == "qwen" {
            QwenConfig::from_env()
                .map(|config| config.model)
                .unwrap_or_else(|_| parts.settings.service.clone())
        } else {
            parts.settings.service.clone()
        };
        let host = MacHost::new(
            parts.ui,
            parts.hub.clone(),
            parts.diagnostics,
            parts.archive,
            parts.recovery,
            parts.ax.clone(),
            parts.settings.service.clone(),
            model,
        );
        let engine = VoiceEngine::new(parts.settings.engine.clone(), host, parts.capture);
        Self {
            engine,
            settings: parts.settings,
            hub: parts.hub,
            corrections: parts.corrections,
            ax: parts.ax,
            commands,
            provider_events,
            provider_tx,
            tokio: parts.tokio,
            metrics: None,
            metrics_failed: false,
            metrics_cancelled: false,
        }
    }

    fn run(mut self) {
        loop {
            let active = self.engine.session_active();
            self.hub
                .set_engine_busy(active || self.engine.provider_alive());
            if active {
                self.engine.poll_capture();
            }

            enum Wake {
                Command(EngineCommand),
                Event(ProviderEvent),
                Timeout,
                Closed,
            }
            let timeout = if active { POLL_ACTIVE } else { POLL_IDLE };
            let wake = select! {
                recv(self.commands) -> message => match message {
                    Ok(command) => Wake::Command(command),
                    Err(_) => Wake::Closed,
                },
                recv(self.provider_events) -> message => match message {
                    Ok(event) => Wake::Event(event),
                    Err(_) => Wake::Closed,
                },
                default(timeout) => Wake::Timeout,
            };

            match wake {
                Wake::Command(EngineCommand::Start { generation, at }) => {
                    self.handle_start(generation, at)
                }
                Wake::Command(EngineCommand::Stop { at }) => {
                    if let Some(metrics) = self.metrics.as_mut() {
                        metrics.mark_key_up(at);
                    }
                    self.hub.begin_transition(clock());
                    self.engine.stop();
                    self.hub.end_transition();
                }
                Wake::Command(EngineCommand::Cancel) => {
                    self.metrics_cancelled = true;
                    self.hub.begin_transition(clock());
                    self.engine.cancel();
                    self.hub.end_transition();
                }
                Wake::Command(EngineCommand::Recover { reason }) => {
                    self.hub.begin_transition(clock());
                    self.engine.recover_runtime(&reason);
                    self.hub.end_transition();
                }
                Wake::Command(EngineCommand::ReloadSettings) => self.reload_settings(),
                Wake::Command(EngineCommand::Shutdown) => {
                    self.flush_metrics();
                    break;
                }
                Wake::Event(event) => self.handle_provider_event(event),
                Wake::Timeout => {}
                Wake::Closed => {
                    self.flush_metrics();
                    break;
                }
            }
        }
    }

    fn handle_start(&mut self, generation: u64, at: f64) {
        if !self.engine.session_active() && !self.engine.provider_alive() {
            self.flush_metrics();
        }
        self.hub.begin_transition(clock());
        self.hub.set_active_generation(generation);

        let frontmost = ax::frontmost_application();
        let frontmost_settings = frontmost
            .as_ref()
            .is_some_and(|app| app.bundle_id == self.settings.settings_bundle_id);
        let target_id = self.ax.capture();
        let setup_trial_target = match (&frontmost, target_id) {
            (Some(app), Some(target)) if frontmost_settings => {
                self.ax.is_setup_trial_target(target, app.pid)
            }
            _ => false,
        };
        let recognition_context = if self.settings.service == "qwen" {
            let config = if self.settings.builtin {
                QwenConfig::for_context()
            } else {
                QwenConfig::from_env()
            };
            config
                .map(|config| config.context_text_with(&self.corrections))
                .unwrap_or_default()
        } else {
            String::new()
        };

        let request = StartRequest {
            now: at,
            target: target_id.map(|id| RuntimeTarget { id }),
            processor_available: self.settings.processor_available,
            recognition_context,
            frontmost_settings,
            setup_trial_target,
            transition_cancelled: self.hub.transition_cancelled(generation),
            update_installing: false,
        };
        if let StartOutcome::Started(stream) = self.engine.start(request) {
            let mut metrics = SessionMetrics::new(stream.session_id.clone(), at);
            metrics.mark_audio_ready(clock());
            self.metrics = Some(metrics);
            self.metrics_failed = false;
            self.metrics_cancelled = false;
            self.spawn_provider(stream);
        }
        self.hub.end_transition();
    }

    fn spawn_provider(&mut self, stream: ProviderStream) {
        let session_id = stream.session_id.clone();
        let events = self.provider_tx.clone();
        if self.settings.builtin {
            let credential = match crate::asr::private_trial::load() {
                Ok(credential) => credential,
                Err(error) => {
                    self.fail_provider(session_id, error.to_string());
                    return;
                }
            };
            match QwenConfig::for_builtin(&credential) {
                Ok(config) => {
                    let mut options = config.request_options();
                    options.context = config.context_text_with(&self.corrections);
                    self.tokio
                        .spawn(run_qwen(stream, config, options, events, super::clock));
                }
                Err(error) => self.fail_provider(
                    session_id,
                    format!("内置服务不可用: {error}"),
                ),
            }
            return;
        }
        match self.settings.service.as_str() {
            "qwen" => match QwenConfig::from_env() {
                Ok(config) => {
                    let mut options = config.request_options();
                    options.context = config.context_text_with(&self.corrections);
                    self.tokio
                        .spawn(run_qwen(stream, config, options, events, super::clock));
                }
                Err(error) => self.fail_provider(
                    session_id,
                    format!("千问流式识别失败: {error}"),
                ),
            },
            "doubao" => match DoubaoConfig::from_env() {
                Ok(config) => {
                    let request = request_settings_from_environment();
                    self.tokio
                        .spawn(run_doubao(stream, config, request, events, super::clock));
                }
                Err(error) => self.fail_provider(session_id, error),
            },
            "custom" => {
                let settings = CustomAsrSettings::from_environment();
                self.tokio.spawn(run_custom(stream, settings, events));
            }
            other => self.fail_provider(
                session_id,
                format!("{other} 服务尚未接入 Rust 引擎"),
            ),
        }
    }

    fn fail_provider(&self, session_id: String, message: String) {
        let _ = self.provider_tx.send(ProviderEvent::Error {
            session_id: session_id.clone(),
            message,
        });
        let _ = self.provider_tx.send(ProviderEvent::Finished { session_id });
    }

    fn handle_provider_event(&mut self, event: ProviderEvent) {
        match event {
            ProviderEvent::Preview {
                session_id,
                text,
                at,
            } => {
                if let Some(metrics) = self.metrics.as_mut() {
                    metrics.mark_partial(at);
                }
                self.engine.provider_preview(&session_id, &text, at)
            }
            ProviderEvent::Final { session_id, text } => {
                self.engine.provider_final(&session_id, &text);
                let committed = self
                    .engine
                    .current_session()
                    .is_some_and(|session| session.final_committed());
                if let Some(metrics) = self.metrics.as_mut() {
                    metrics.mark_final(clock(), committed);
                }
            }
            ProviderEvent::Complete { session_id } => {
                self.engine.provider_complete(&session_id)
            }
            ProviderEvent::Error {
                session_id,
                message,
            } => {
                self.metrics_failed = true;
                self.engine.provider_error(&session_id, &message)
            }
            ProviderEvent::Finished { session_id } => {
                self.engine.provider_finished(&session_id);
                self.flush_metrics();
            }
        }
    }

    fn flush_metrics(&mut self) {
        let Some(metrics) = self.metrics.take() else {
            return;
        };
        let outcome = if self.metrics_cancelled {
            "cancelled"
        } else if self.metrics_failed {
            "error"
        } else if metrics.committed() {
            "committed"
        } else if metrics.received_final() {
            "not_committed"
        } else {
            "no_final"
        };
        metrics::append(&metrics.snapshot(outcome));
        self.metrics_failed = false;
        self.metrics_cancelled = false;
    }

    fn reload_settings(&mut self) {
        let settings = RuntimeSettings::load();
        self.engine.set_config(settings.engine.clone());
        self.engine
            .host_mut()
            .set_engine_identity(settings.service.clone());
        self.settings = settings;
    }
}
