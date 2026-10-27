//! Process-level wiring shared by the headless run and the menu-bar app.
//!
//! Owns the instance lock, engine actor, hotkey listener, permission monitor,
//! health checks, and the settings/microphone/reconnect marker protocol.

use std::ffi::CString;
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use super::actor::{EngineActorHandle, EngineActorParts};
use super::hub::StateHub;
use super::sink::TransitionSink;
use super::{clock, EngineCommand};
use crate::audio::archive::AudioArchiveManager;
use crate::audio::capture::CpalCapture;
use crate::config;
use crate::diagnostics::DiagnosticsStore;
use crate::engine::AudioCapture;
use crate::engine_ipc::{
    self, microphone_test_audible, MicrophoneTestDecision, PermissionSnapshot, ReconnectDecision,
    ReconnectInput, SettingsApplyDecision, SettingsApplied,
};
use crate::health::{self, RuntimeHealth};
use crate::hotkey::backend_for_hotkey;
use crate::hotkey::listener::{ListenerHandle, ListenerHeartbeat, SystemKeyState};
use crate::insert::ax::AxClient;
use crate::insert::corrections::CorrectionStore;
use crate::insert::recovery::RecoveryStore;
use crate::instance_lock::AppInstance;
use crate::login_item::{self, LoginItemResult};
use crate::paths;
use crate::permissions::PermissionMonitor;
use crate::session::{health_fault, HealthInput};
use crate::settings::RuntimeSettings;
use crate::settings_launch::SettingsLauncher;
use crate::state::InputState;
use crate::ui::{NullUiSink, UiSink};

const TICK_INTERVAL: Duration = Duration::from_secs(2);
const MICROPHONE_TEST_SECONDS: u64 = 2;
const HEALTH_INTERVAL: Duration = Duration::from_secs(2);

type ListenerSlot = Arc<Mutex<Option<ListenerHeartbeat>>>;

static STOP_SIGNAL: AtomicBool = AtomicBool::new(false);

extern "C" fn request_stop(_signal: libc::c_int) {
    STOP_SIGNAL.store(true, Ordering::SeqCst);
}

pub(crate) fn install_stop_signals() {
    unsafe {
        libc::signal(
            libc::SIGINT,
            request_stop as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            request_stop as *const () as libc::sighandler_t,
        );
    }
}

pub(crate) fn stop_requested() -> bool {
    STOP_SIGNAL.load(Ordering::SeqCst)
}

pub struct AppRuntime {
    hub: Arc<StateHub>,
    commands: crossbeam_channel::Sender<EngineCommand>,
    actor: Option<EngineActorHandle>,
    listener: Option<ListenerHandle>,
    listener_slot: ListenerSlot,
    settings: RuntimeSettings,
    sink: TransitionSink,
    diagnostics: Arc<DiagnosticsStore>,
    permission_monitor: Arc<PermissionMonitor>,
    health: Arc<Mutex<RuntimeHealth>>,
    health_thread: Option<JoinHandle<()>>,
    microphone_thread: Option<JoinHandle<()>>,
    last_permission: Option<PermissionSnapshot>,
    login: LoginItemResult,
    stop: Arc<AtomicBool>,
    tokio: Option<tokio::runtime::Runtime>,
    settings_launcher: SettingsLauncher,
    instance: AppInstance,
}

impl AppRuntime {
    /// `None` means another process already owns this data directory.
    pub fn start(ui: Arc<dyn UiSink>) -> Result<Option<Self>> {
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        login_item::resume_supervisor_for_manual_launch(&arguments);
        paths::ensure_runtime_layout()?;
        let Some(instance) = AppInstance::acquire(&paths::data_dir())? else {
            return Ok(None);
        };

        let settings = RuntimeSettings::load();
        let login = login_item::sync_login_item(login_enabled());
        let hub = Arc::new(StateHub::new(clock()));
        let diagnostics = Arc::new(DiagnosticsStore::new(
            Some(paths::diagnostics_file()),
            true,
        ));
        let recovery = Arc::new(RecoveryStore::new(paths::recovery_file(), 5));
        let corrections = Arc::new(CorrectionStore::new(Some(paths::corrections_file())));
        let ax = AxClient::spawn_with_corrections(recovery.clone(), corrections.clone());
        let archive = AudioArchiveManager::new(Some(&paths::audio_archive_dir()));
        let capture = CpalCapture::from_env();

        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .context("无法启动网络运行时")?;

        let actor = EngineActorHandle::spawn(EngineActorParts {
            settings: settings.clone(),
            hub: hub.clone(),
            ui,
            diagnostics: diagnostics.clone(),
            recovery,
            corrections,
            archive,
            ax,
            capture,
            tokio: tokio.handle().clone(),
        });
        let commands = actor.sender();
        let sink = actor.sink(hub.clone());

        let listener_slot: ListenerSlot = Arc::new(Mutex::new(None));
        let backend = permission_backend(&settings);
        let ready_slot = listener_slot.clone();
        let listener_ready: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            ready_slot
                .lock()
                .ok()
                .and_then(|slot| slot.as_ref().map(ListenerHeartbeat::available))
                .unwrap_or(false)
        });
        let permission_monitor = Arc::new(PermissionMonitor::new(
            crate::paths::app_version(),
            backend,
            None,
            None,
            2.0,
            Some(listener_ready),
        ));

        let listener = spawn_listener(&settings, sink.clone(), &diagnostics);
        if let Some(handle) = listener.as_ref() {
            *listener_slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some(handle.heartbeat());
        }

        let stop = Arc::new(AtomicBool::new(false));
        let health = Arc::new(Mutex::new(RuntimeHealth::new(
            health_probe(hub.clone(), listener_slot.clone()),
            health_recover(commands.clone(), diagnostics.clone()),
            Some(paths::health_file()),
        )));
        let health_thread = spawn_health(health.clone(), stop.clone());

        let runtime = Self {
            hub,
            commands,
            actor: Some(actor),
            listener,
            listener_slot,
            settings,
            sink,
            diagnostics,
            permission_monitor,
            health,
            health_thread,
            microphone_thread: None,
            last_permission: None,
            login,
            stop,
            tokio: Some(tokio),
            settings_launcher: SettingsLauncher::new(),
            instance,
        };
        runtime.write_startup_diagnostics();
        runtime.permission_monitor.start();
        Ok(Some(runtime))
    }

    /// Foreground entry point used by the `run` subcommand.
    pub fn run_headless() -> Result<()> {
        install_stop_signals();
        let Some(mut runtime) = Self::start(Arc::new(NullUiSink))? else {
            eprintln!("已有 Veyqa Voice 正在使用该数据目录，跳过重复启动");
            return Ok(());
        };
        while !STOP_SIGNAL.load(Ordering::SeqCst) && !runtime.stop.load(Ordering::SeqCst) {
            runtime.tick();
            if STOP_SIGNAL.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(TICK_INTERVAL);
        }
        runtime.shutdown();
        Ok(())
    }

    pub fn hub(&self) -> Arc<StateHub> {
        self.hub.clone()
    }

    /// Menu-bar entry point. Must run on the AppKit main thread.
    pub fn open_settings(&mut self) -> Result<()> {
        if let Err(error) = self.settings_launcher.open() {
            let _ = self.diagnostics.update(&Map::from_iter([(
                "last_error".to_string(),
                json!(error.to_string()),
            )]));
            return Err(error);
        }
        Ok(())
    }

    /// Mirror `_should_open_settings_on_start` for the launch environment.
    pub fn should_open_settings_on_start(&self) -> bool {
        let arguments: Vec<String> = std::env::args().collect();
        let restarting = std::env::var("VOICE_INPUT_RESTARTING")
            .map(|value| value == "true")
            .unwrap_or(false);
        let setup_complete = std::env::var("SETUP_COMPLETED")
            .map(|value| value.to_lowercase() == "true")
            .unwrap_or(true);
        crate::settings_launch::should_open_settings_on_start(
            self.settings.processor_available,
            paths::is_frozen(),
            &arguments,
            restarting,
            setup_complete,
        )
    }

    /// One health tick, also used by the future AppKit common-mode timer.
    pub fn tick(&mut self) {
        if let Ok(mut health) = self.health.lock() {
            health.tick_main();
        }
        self.apply_pending_settings();
        self.refresh_permission_listener();
        self.check_microphone_test_request();
        self.check_promotion_request();
        self.handle_restart_request();
    }

    /// The settings helper writes a request id when the user presses "检查"
    /// for the built-in service; the engine answers through `promotion.json`.
    fn check_promotion_request(&mut self) {
        let request = paths::promotion_prefetch_request_file();
        if !request.exists() {
            return;
        }
        let request_id: String = std::fs::read_to_string(&request)
            .unwrap_or_default()
            .chars()
            .take(80)
            .collect();
        let request_id = request_id.trim().to_string();
        engine_ipc::consume_marker(&request);
        if !crate::release_features::builtin_speech_enabled() || request_id.len() != 36 {
            return;
        }
        if let Some(tokio) = &self.tokio {
            if crate::release_features::private_trial_enabled() {
                tokio.spawn(crate::asr::private_trial::check_and_publish(request_id));
            }
        }
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.permission_monitor.stop();
        if let Some(mut listener) = self.listener.take() {
            listener.stop();
        }
        if let Some(actor) = self.actor.take() {
            let mut actor = actor;
            actor.shutdown();
        }
        if let Some(thread) = self.health_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.microphone_thread.take() {
            let _ = thread.join();
        }
        if let Some(tokio) = self.tokio.take() {
            tokio.shutdown_timeout(Duration::from_secs(1));
        }
        // Closing the instance lock last keeps a duplicate launch out until the
        // microphone and hotkey owners are gone.
        let _ = &self.instance;
    }

    fn write_startup_diagnostics(&self) {
        let mut values = Map::new();
        values.insert("version".into(), json!(crate::paths::app_version()));
        values.insert("engine".into(), json!(self.settings.service));
        values.insert("model".into(), json!(self.settings.model_name()));
        values.insert("state".into(), json!(InputState::Idle.name()));
        values.insert("microphone".into(), json!("未知"));
        values.insert("sample_rate".into(), Value::Null);
        values.insert("hotkey".into(), json!(self.settings.hotkey_label));
        values.insert(
            "hotkey_backend".into(),
            json!(permission_backend(&self.settings)),
        );
        values.insert(
            "archive_enabled".into(),
            json!(self.settings.engine.archive_enabled),
        );
        values.insert("last_error".into(), json!(self.login.error));
        values.insert("login_item_status".into(), json!(self.login.status));
        values.insert("login_item_error".into(), json!(self.login.error));
        values.insert("recovery_status".into(), json!("后台服务就绪"));
        let _ = self.diagnostics.update(&values);
    }

    fn apply_pending_settings(&mut self) {
        let path = engine_ipc::settings_apply_request();
        let decision = engine_ipc::settings_apply_decision(
            &path,
            self.hub.busy(),
            self.hub.is_recording(),
        );
        match decision {
            SettingsApplyDecision::Ignore | SettingsApplyDecision::DeferRecording => {}
            SettingsApplyDecision::Pending(revision) => {
                let _ = self
                    .diagnostics
                    .update(&engine_ipc::settings_pending_values(&revision));
            }
            SettingsApplyDecision::Apply(revision) => {
                if let Err(error) = self.apply_settings_revision(&path, &revision) {
                    let _ = self
                        .diagnostics
                        .update(&engine_ipc::settings_error_values(&revision));
                    let mut values = Map::new();
                    values.insert("last_error".into(), json!(error.to_string()));
                    let _ = self.diagnostics.update(&values);
                    engine_ipc::finish_settings_apply(&path, &revision);
                }
            }
        }
    }

    fn apply_settings_revision(&mut self, path: &Path, revision: &str) -> Result<()> {
        config::init().context("无法重新读取设置")?;
        crate::settings::adopt_env_hotkey();
        let settings = RuntimeSettings::load();
        if !settings.processor_available {
            anyhow::bail!("语音服务凭证尚未配置");
        }
        self.permission_monitor
            .set_hotkey_backend(permission_backend(&settings));
        let _ = self.commands.send(EngineCommand::ReloadSettings);
        self.settings = settings;
        self.rebuild_listener("权限已更新，快捷键监听已重新启用");

        self.login = login_item::sync_login_item(login_enabled());
        let values = engine_ipc::settings_applied_values(&SettingsApplied {
            revision,
            engine: &self.settings.service,
            model: &self.settings.model_name(),
            hotkey: &self.settings.hotkey_label,
            login_item_status: &self.login.status,
            login_item_error: &self.login.error,
        });
        self.diagnostics.update(&values)?;
        engine_ipc::finish_settings_apply(path, revision);
        Ok(())
    }

    fn refresh_permission_listener(&mut self) {
        let Some(snapshot) = self.permission_monitor.latest_snapshot() else {
            return;
        };
        let current = PermissionSnapshot {
            accessibility: snapshot
                .get("accessibility")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input_monitoring: snapshot
                .get("input_monitoring")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        };
        let reconnect = paths::listener_reconnect_request_file();
        let decision = engine_ipc::refresh_permission_listener(&ReconnectInput {
            current: &current,
            previous: self.last_permission.as_ref(),
            requested: reconnect.exists(),
            busy: self.hub.busy(),
            hotkey_backend: permission_backend(&self.settings),
        });
        match decision {
            ReconnectDecision::NoChange | ReconnectDecision::Deferred => {}
            ReconnectDecision::Rebind => {
                self.last_permission = Some(current);
                engine_ipc::consume_marker(&reconnect);
                self.rebuild_listener("权限已更新，快捷键监听已重新启用");
            }
            ReconnectDecision::Acknowledged => {
                self.last_permission = Some(current);
                engine_ipc::consume_marker(&reconnect);
            }
        }
    }

    fn rebuild_listener(&mut self, success_status: &str) {
        if let Some(mut listener) = self.listener.take() {
            listener.stop();
        }
        let listener = spawn_listener(&self.settings, self.sink.clone(), &self.diagnostics);
        let mut slot = self
            .listener_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = listener.as_ref().map(ListenerHandle::heartbeat);
        self.listener = listener;

        let mut values = Map::new();
        values.insert(
            "hotkey_backend".into(),
            json!(permission_backend(&self.settings)),
        );
        if self.listener.is_some() {
            values.insert("recovery_status".into(), json!(success_status));
        }
        let _ = self.diagnostics.update(&values);
    }

    fn check_microphone_test_request(&mut self) {
        let request = paths::microphone_test_request_file();
        let active = self.hub.microphone_test_active();
        let decision = engine_ipc::microphone_test_decision(
            request.exists(),
            active,
            self.hub.engine_busy() || self.hub.is_recording(),
        );
        match decision {
            MicrophoneTestDecision::Ignore => {}
            MicrophoneTestDecision::RejectBusy => {
                engine_ipc::consume_marker(&request);
                let _ = self.diagnostics.update(&Map::from_iter([(
                    "microphone_test_status".to_string(),
                    json!(engine_ipc::MICROPHONE_TEST_BUSY_STATUS),
                )]));
            }
            MicrophoneTestDecision::Start => {
                engine_ipc::consume_marker(&request);
                if self.hub.microphone_test_active() {
                    return;
                }
                self.hub.set_microphone_test_active(true);
                let _ = self.diagnostics.update(&Map::from_iter([(
                    "microphone_test_status".to_string(),
                    json!(engine_ipc::MICROPHONE_TEST_PROGRESS_STATUS),
                )]));
                let diagnostics = self.diagnostics.clone();
                let hub = self.hub.clone();
                self.microphone_thread = thread::Builder::new()
                    .name("veyqa-microphone-test".to_string())
                    .spawn(move || {
                        let status = run_microphone_test();
                        let _ = diagnostics.update(&Map::from_iter([(
                            "microphone_test_status".to_string(),
                            json!(status),
                        )]));
                        hub.set_microphone_test_active(false);
                    })
                    .ok();
            }
        }
    }

    fn handle_restart_request(&mut self) {
        let request = paths::restart_request_file();
        if !request.exists() {
            return;
        }
        let background = std::fs::read_to_string(&request)
            .map(|text| text.trim() == "background")
            .unwrap_or(false);
        engine_ipc::consume_marker(&request);
        if !restart_application(!background) {
            let _ = self.diagnostics.update(&Map::from_iter([(
                "recovery_status".to_string(),
                json!("开发模式未自动重启，请手动重新启动"),
            )]));
        }
    }
}

impl Drop for AppRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn health_probe(hub: Arc<StateHub>, slot: ListenerSlot) -> health::Probe {
    Box::new(move |now| {
        let listener_last_poll = slot.lock().ok().and_then(|slot| {
            slot.as_ref()
                .map(|heartbeat| now - heartbeat.age().as_secs_f64())
        });
        health_fault(
            now,
            &HealthInput {
                transition_started_at: hub.transition_started_at(),
                state: hub.state(),
                state_since: hub.state_since(),
                listener_last_poll,
            },
        )
        .map(str::to_string)
    })
}

fn health_recover(
    commands: crossbeam_channel::Sender<EngineCommand>,
    diagnostics: Arc<DiagnosticsStore>,
) -> health::Recover {
    Box::new(move |reason| {
        let _ = commands.send(EngineCommand::Recover {
            reason: reason.to_string(),
        });
        let allowed = health::allow_automatic_restart(None, None).unwrap_or(false);
        let mut values = Map::new();
        values.insert("last_error".into(), json!(reason));
        values.insert(
            "recovery_status".into(),
            json!(if allowed {
                "正在自动恢复"
            } else {
                "恢复次数过多，请检查音频设备后重新启动"
            }),
        );
        let _ = diagnostics.update(&values);
        if allowed {
            let _ = std::fs::write(paths::restart_request_file(), "background");
        }
    })
}

fn spawn_health(health: Arc<Mutex<RuntimeHealth>>, stop: Arc<AtomicBool>) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name("veyqa-health".to_string())
        .spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                thread::sleep(HEALTH_INTERVAL);
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(mut health) = health.lock() {
                    let _ = health.poll();
                }
            }
        })
        .ok()
}

fn spawn_listener(
    settings: &RuntimeSettings,
    sink: TransitionSink,
    diagnostics: &DiagnosticsStore,
) -> Option<ListenerHandle> {
    let backend = permission_backend(settings);
    if backend == "off" {
        return None;
    }
    let capture = paths::shortcut_capture_file();
    let result = if backend == "registered" {
        ListenerHandle::spawn_registered(
            settings.hotkey.clone(),
            settings.hotkey_mode,
            capture,
            Box::new(sink),
        )
    } else {
        match ListenerHandle::spawn_passive(
            settings.hotkey.clone(),
            settings.hotkey_mode,
            capture.clone(),
            Box::new(sink.clone()),
            Box::new(SystemKeyState),
        ) {
            Ok(handle) => Ok(handle),
            Err(tap_error) => ListenerHandle::spawn_monitor(
                settings.hotkey.clone(),
                settings.hotkey_mode,
                capture,
                Box::new(sink),
            )
            .map_err(|monitor_error| {
                anyhow::anyhow!("{tap_error}；兼容监听也失败：{monitor_error}")
            }),
        }
    };
    match result {
        Ok(handle) => Some(handle),
        Err(error) => {
            let mut values = Map::new();
            values.insert("last_error".into(), json!(error.to_string()));
            values.insert(
                "recovery_status".into(),
                json!("快捷键监听未启动，请检查“输入监控”授权"),
            );
            let _ = diagnostics.update(&values);
            None
        }
    }
}

/// Mirror `transcription.promotion.uses_builtin` absent: the UI backend value.
fn permission_backend(settings: &RuntimeSettings) -> &'static str {
    let requested = config::var_or("GLOBAL_HOTKEY_BACKEND", "passive");
    backend_for_hotkey(Some(&settings.hotkey), &requested)
}

fn login_enabled() -> bool {
    config::bool_var("LAUNCH_AT_LOGIN", true)
}

fn run_microphone_test() -> String {
    let mut capture = CpalCapture::from_env();
    if let Err(error) = capture.start() {
        return error;
    }
    thread::sleep(Duration::from_secs(MICROPHONE_TEST_SECONDS));
    let wav = capture.stop(false);
    capture.reset_streaming_state();
    if wav_is_audible(wav.as_deref()) {
        "passed".to_string()
    } else {
        "未收到声音，请检查系统声音输入设备和音量".to_string()
    }
}

fn wav_is_audible(wav: Option<&[u8]>) -> bool {
    let Some(bytes) = wav else {
        return false;
    };
    let Ok(mut reader) = hound::WavReader::new(Cursor::new(bytes)) else {
        return false;
    };
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .filter_map(Result::ok)
            .map(|value| value as f32 / 32768.0)
            .collect(),
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(Result::ok).collect(),
    };
    microphone_test_audible(&samples)
}

/// Replace the process image. Refused in a development build unless the
/// caller opts in, so a fault loop cannot fight the terminal session.
pub fn restart_application(show_settings: bool) -> bool {
    if !paths::is_frozen() && !config::bool_var("VEYQA_ALLOW_DEV_RESTART", false) {
        return false;
    }
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let flag = if show_settings {
        "--show-settings-after-restart"
    } else {
        "--background-login"
    };
    let mut arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    arguments.retain(|argument| {
        argument != "--show-settings-after-restart" && argument != "--background-login"
    });
    let mut argv: Vec<CString> = Vec::new();
    argv.push(CString::new(executable.as_os_str().as_encoded_bytes()).unwrap_or_default());
    argv.push(CString::new(flag).unwrap_or_default());
    for argument in &arguments {
        argv.push(CString::new(argument.as_encoded_bytes()).unwrap_or_default());
    }
    let mut pointers: Vec<*const libc::c_char> = argv.iter().map(|item| item.as_ptr()).collect();
    pointers.push(std::ptr::null());
    unsafe {
        libc::execv(argv[0].as_ptr(), pointers.as_ptr());
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_of(samples: &[i16]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::new(Cursor::new(&mut bytes), spec).unwrap();
            for sample in samples {
                writer.write_sample(*sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        bytes
    }

    #[test]
    fn microphone_test_wav_threshold_matches_python() {
        assert!(!wav_is_audible(None));
        assert!(!wav_is_audible(Some(&wav_of(&[0; 1600]))));
        assert!(!wav_is_audible(Some(&wav_of(&[32; 1600]))));
        assert!(wav_is_audible(Some(&wav_of(&[600; 1600]))));
        assert!(!wav_is_audible(Some(b"not a wav")));
    }

    #[test]
    fn health_probe_reports_a_stuck_transition() {
        let hub = Arc::new(StateHub::new(0.0));
        let slot: ListenerSlot = Arc::new(Mutex::new(None));
        let mut probe = health_probe(hub.clone(), slot);
        assert_eq!(probe(1.0), None);
        hub.begin_transition(1.0);
        assert_eq!(probe(12.0), Some("录音设备操作超时".to_string()));
        hub.end_transition();
    }

    #[test]
    fn health_probe_ignores_a_quiet_engine_without_a_listener() {
        let hub = Arc::new(StateHub::new(0.0));
        let slot: ListenerSlot = Arc::new(Mutex::new(None));
        let mut probe = health_probe(hub, slot);
        assert_eq!(probe(100.0), None);
    }

    #[test]
    fn stop_signal_flag_is_process_wide() {
        STOP_SIGNAL.store(false, Ordering::SeqCst);
        request_stop(libc::SIGTERM);
        assert!(STOP_SIGNAL.load(Ordering::SeqCst));
        STOP_SIGNAL.store(false, Ordering::SeqCst);
    }
}
