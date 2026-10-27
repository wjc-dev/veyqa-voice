//! Production microphone capture on cpal.
//!
//! One session owns one dedicated thread: it selects the device, builds and
//! plays the stream, then pumps mono samples through `PcmChunker` into 100 ms
//! 16 kHz PCM16 chunks. The engine consumes chunks through `poll_events`,
//! matching the Python recorder's queue while keeping start/stop timeouts and
//! device errors out of the hotkey path.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, Sample, SampleFormat, SizedSample, StreamConfig};

use super::devices::select_input_device;
use super::policy::{
    self, MAX_RECORD_SECONDS, START_TIMEOUT_ERROR, STOP_TIMEOUT_SECONDS,
};
use super::resample::PcmChunker;
use crate::config;
use crate::engine::AudioCapture;

const TARGET_RATE: u32 = 16_000;
const CHUNK_MS: u32 = 100;
/// Reusable callback buffers; a full pool means "drop this callback".
const POOL_BUFFERS: usize = 4;

#[derive(Debug, Clone, PartialEq)]
pub enum CaptureEvent {
    /// 16 kHz mono PCM16 little-endian, one block per 100 ms.
    Chunk(Vec<u8>),
    /// Ten minutes reached; submit what was captured.
    AutoStop,
    /// The stream reported a device loss; commit the audio already captured.
    DeviceError(String),
}

struct Control {
    recording: AtomicBool,
    stop: AtomicBool,
    abort: AtomicBool,
    abandoned: AtomicBool,
    auto_stop_sent: AtomicBool,
    error_sent: AtomicBool,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            recording: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            abort: AtomicBool::new(false),
            abandoned: AtomicBool::new(false),
            auto_stop_sent: AtomicBool::new(false),
            error_sent: AtomicBool::new(false),
        }
    }
}

enum StartResult {
    Started { device: String, rate: u32 },
    Failed(String),
}

struct StopResult {
    audio: Option<Vec<u8>>,
}

struct ActiveSession {
    control: Arc<Control>,
    done: Receiver<StopResult>,
    recorded: Arc<Mutex<Vec<f32>>>,
    rate: u32,
    thread: Option<JoinHandle<()>>,
}

pub struct CpalCapture {
    device_override: String,
    device_policy: String,
    post_roll_ms: u32,
    anti_alias: bool,
    agc: bool,
    events_tx: crossbeam_channel::Sender<CaptureEvent>,
    events_rx: crossbeam_channel::Receiver<CaptureEvent>,
    last_error: Arc<Mutex<String>>,
    session: Option<ActiveSession>,
    current_device: Option<String>,
    sample_rate: Option<u32>,
    last_used_device: Option<String>,
    finished: Vec<JoinHandle<()>>,
}

impl CpalCapture {
    pub fn from_env() -> Self {
        Self {
            anti_alias: config::bool_var("AUDIO_ANTI_ALIAS_ENABLED", true),
            agc: config::bool_var("AUDIO_AGC_ENABLED", true),
            ..Self::new(
                config::var("AUDIO_INPUT_DEVICE"),
                config::var_or("AUDIO_DEVICE_POLICY", "system"),
                policy::post_roll_ms(),
            )
        }
    }

    pub fn new(
        device_override: String,
        device_policy: String,
        post_roll_ms: u32,
    ) -> Self {
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        Self {
            device_override,
            device_policy,
            post_roll_ms,
            anti_alias: true,
            agc: true,
            events_tx,
            events_rx,
            last_error: Arc::new(Mutex::new(String::new())),
            session: None,
            current_device: None,
            sample_rate: None,
            last_used_device: None,
            finished: Vec::new(),
        }
    }

    fn reap_finished(&mut self) {
        let mut pending = std::mem::take(&mut self.finished);
        let mut keep = Vec::new();
        for handle in pending.drain(..) {
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                keep.push(handle);
            }
        }
        self.finished = keep;
    }

    fn clear_error(&self) {
        if let Ok(mut error) = self.last_error.lock() {
            error.clear();
        }
    }

    fn captured_wav(&self, recorded: &Mutex<Vec<f32>>, rate: u32) -> Option<Vec<u8>> {
        let samples = recorded
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        (!samples.is_empty()).then(|| wav_bytes(&samples, rate))
    }
}

impl Default for CpalCapture {
    fn default() -> Self {
        Self::from_env()
    }
}

impl AudioCapture for CpalCapture {
    fn start(&mut self) -> Result<(), String> {
        if self.session.is_some() {
            self.reset_streaming_state();
        }
        self.reap_finished();
        self.clear_error();
        while self.events_rx.try_recv().is_ok() {}

        let control = Arc::new(Control::default());
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let last_error = Arc::clone(&self.last_error);
        let events = self.events_tx.clone();
        let device_override = self.device_override.clone();
        let device_policy = self.device_policy.clone();
        let anti_alias = self.anti_alias;
        let agc = self.agc;
        let thread_control = Arc::clone(&control);
        let thread_recorded = Arc::clone(&recorded);
        let (start_tx, start_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        let handle = std::thread::Builder::new()
            .name("veyqa-capture".into())
            .spawn(move || {
                run_capture_thread(
                    thread_control,
                    thread_recorded,
                    last_error,
                    events,
                    device_override,
                    device_policy,
                    anti_alias,
                    agc,
                    start_tx,
                    done_tx,
                );
            })
            .map_err(|error| format!("录音线程无法启动: {error}"))?;

        let timeout = Duration::from_millis((policy::START_TIMEOUT_SECONDS * 1000.0) as u64);
        match start_rx.recv_timeout(timeout) {
            Ok(StartResult::Started { device, rate }) => {
                self.current_device = Some(device.clone());
                self.last_used_device = Some(device);
                self.sample_rate = Some(rate);
                self.session = Some(ActiveSession {
                    control,
                    done: done_rx,
                    recorded,
                    rate,
                    thread: Some(handle),
                });
                Ok(())
            }
            Ok(StartResult::Failed(message)) => {
                let _ = handle.join();
                Err(message)
            }
            Err(_) => {
                // A blocked driver must not trap the hotkey; abandon the thread.
                control.abandoned.store(true, Ordering::Release);
                control.recording.store(false, Ordering::Release);
                control.stop.store(true, Ordering::Release);
                self.finished.push(handle);
                Err(START_TIMEOUT_ERROR.to_string())
            }
        }
    }

    fn stop(&mut self, abort: bool) -> Option<Vec<u8>> {
        let Some(mut session) = self.session.take() else {
            return None;
        };
        if !abort && self.post_roll_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.post_roll_ms as u64));
        }
        session.control.abort.store(abort, Ordering::Release);
        session.control.recording.store(false, Ordering::Release);
        session.control.stop.store(true, Ordering::Release);

        let timeout = Duration::from_millis((STOP_TIMEOUT_SECONDS * 1000.0) as u64);
        let audio = match session.done.recv_timeout(timeout) {
            Ok(result) => result.audio,
            Err(RecvTimeoutError::Disconnected) => {
                if abort {
                    None
                } else {
                    self.captured_wav(&session.recorded, session.rate)
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // Everything captured so far remains usable even if stop hung.
                if abort {
                    None
                } else {
                    self.captured_wav(&session.recorded, session.rate)
                }
            }
        };
        if let Some(handle) = session.thread.take() {
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                self.finished.push(handle);
            }
        }
        audio
    }

    fn reset_streaming_state(&mut self) {
        self.stop(true);
        while self.events_rx.try_recv().is_ok() {}
        self.clear_error();
        self.reap_finished();
    }

    fn current_device(&self) -> Option<String> {
        self.current_device.clone()
    }

    fn sample_rate(&self) -> Option<u32> {
        self.sample_rate
    }

    fn capture_error(&self) -> String {
        self.last_error
            .lock()
            .map(|error| error.clone())
            .unwrap_or_default()
    }

    fn poll_events(&mut self) -> Vec<CaptureEvent> {
        self.events_rx.try_iter().collect()
    }
}

/// Pair a 16 kHz rate with the format that actually supports it, or fall back
/// to the device default so resampling handles the difference.
fn choose_config(
    device: &cpal::Device,
) -> Result<(StreamConfig, SampleFormat, u32), cpal::Error> {
    if let Ok(ranges) = device.supported_input_configs() {
        for range in ranges {
            if range.min_sample_rate() <= TARGET_RATE
                && TARGET_RATE <= range.max_sample_rate()
            {
                return Ok((
                    StreamConfig {
                        channels: range.channels(),
                        sample_rate: TARGET_RATE,
                        buffer_size: cpal::BufferSize::Default,
                    },
                    range.sample_format(),
                    TARGET_RATE,
                ));
            }
        }
    }
    let supported = device.default_input_config()?;
    let format = supported.sample_format();
    let rate = supported.sample_rate();
    Ok((supported.config(), format, rate))
}

#[allow(clippy::too_many_arguments)]
fn run_capture_thread(
    control: Arc<Control>,
    recorded: Arc<Mutex<Vec<f32>>>,
    last_error: Arc<Mutex<String>>,
    events: crossbeam_channel::Sender<CaptureEvent>,
    device_override: String,
    device_policy: String,
    anti_alias: bool,
    agc: bool,
    start_tx: Sender<StartResult>,
    done_tx: Sender<StopResult>,
) {
    let host = cpal::default_host();

    let (free_tx, free_rx) = crossbeam_channel::bounded::<Vec<f32>>(POOL_BUFFERS);
    for _ in 0..POOL_BUFFERS {
        let _ = free_tx.try_send(Vec::with_capacity(8192));
    }
    let (filled_tx, filled_rx) = crossbeam_channel::bounded::<Vec<f32>>(POOL_BUFFERS + 4);
    let (error_tx, error_rx) = crossbeam_channel::bounded::<String>(4);

    let setup = (|| -> Result<(cpal::Stream, String, u32), String> {
        let (name, device) =
            select_input_device(&host, &device_override, &device_policy)
                .map_err(|error| error.to_string())?;
        let (config, format, rate) =
            choose_config(&device).map_err(|error| error.to_string())?;
        let stream = match format {
            SampleFormat::F32 => build_stream::<f32>,
            SampleFormat::I16 => build_stream::<i16>,
            SampleFormat::U16 => build_stream::<u16>,
            SampleFormat::I32 => build_stream::<i32>,
            other => {
                return Err(format!("暂不支持的采样格式: {other:?}"));
            }
        }(
            &device, &config, &control, &free_rx, &filled_tx, &error_tx,
        )
        .map_err(|error| format!("无法启动音频采集: {error}"))?;
        stream
            .play()
            .map_err(|error| format!("麦克风启动失败，请检查设备与权限: {error}"))?;
        Ok((stream, name, rate))
    })();

    let (stream, device, rate) = match setup {
        Ok(setup) => setup,
        Err(message) => {
            let _ = start_tx.send(StartResult::Failed(message));
            return;
        }
    };

    if control.abandoned.load(Ordering::Acquire) {
        drop(stream);
        return;
    }
    if start_tx
        .send(StartResult::Started { device, rate })
        .is_err()
    {
        drop(stream);
        return;
    }

    bridge_audio(
        &control,
        &recorded,
        &last_error,
        &events,
        rate,
        anti_alias,
        agc,
        &filled_rx,
        &free_tx,
        &error_rx,
        &done_tx,
    );
    // Releasing a wedged CoreAudio stream must never delay the engine.
    drop(stream);
}

#[allow(clippy::too_many_arguments)]
fn bridge_audio(
    control: &Control,
    recorded: &Mutex<Vec<f32>>,
    last_error: &Mutex<String>,
    events: &crossbeam_channel::Sender<CaptureEvent>,
    rate: u32,
    anti_alias: bool,
    agc: bool,
    filled: &crossbeam_channel::Receiver<Vec<f32>>,
    free: &crossbeam_channel::Sender<Vec<f32>>,
    errors: &crossbeam_channel::Receiver<String>,
    done: &Sender<StopResult>,
) {
    let mut chunker = PcmChunker::with_processing(rate, TARGET_RATE, CHUNK_MS, anti_alias, agc);
    let started = Instant::now();

    loop {
        if let Ok(message) = errors.try_recv() {
            if let Ok(mut slot) = last_error.lock() {
                *slot = message.clone();
            }
            let _ = events.send(CaptureEvent::DeviceError(message));
            break;
        }
        if control.stop.load(Ordering::Acquire) {
            break;
        }
        match filled.recv_timeout(Duration::from_millis(50)) {
            Ok(mut buffer) => {
                if control.recording.load(Ordering::Acquire) {
                    if let Ok(mut total) = recorded.lock() {
                        total.extend_from_slice(&buffer);
                    }
                    if let Some(chunk) = chunker.push(&buffer) {
                        let _ = events.send(CaptureEvent::Chunk(chunk));
                    }
                }
                buffer.clear();
                let _ = free.try_send(buffer);
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
        if started.elapsed().as_secs_f64() >= MAX_RECORD_SECONDS
            && !control.auto_stop_sent.swap(true, Ordering::AcqRel)
        {
            let _ = events.send(CaptureEvent::AutoStop);
        }
    }

    // Every buffer here was captured before the stream stopped.
    while let Ok(mut buffer) = filled.try_recv() {
        if let Ok(mut total) = recorded.lock() {
            total.extend_from_slice(&buffer);
        }
        if let Some(chunk) = chunker.push(&buffer) {
            let _ = events.send(CaptureEvent::Chunk(chunk));
        }
        buffer.clear();
        let _ = free.try_send(buffer);
    }

    let abort = control.abort.load(Ordering::Acquire);
    let audio = if abort {
        if let Ok(mut total) = recorded.lock() {
            total.clear();
        }
        None
    } else {
        if let Some(tail) = chunker.finish() {
            let _ = events.send(CaptureEvent::Chunk(tail));
        }
        let samples = recorded
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        if samples.is_empty() {
            None
        } else {
            Some(wav_bytes(&samples, rate))
        }
    };
    let _ = done.send(StopResult { audio });
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    control: &Arc<Control>,
    free: &crossbeam_channel::Receiver<Vec<f32>>,
    filled: &crossbeam_channel::Sender<Vec<f32>>,
    errors: &crossbeam_channel::Sender<String>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + 'static,
    f32: FromSample<T>,
{
    let control = Arc::clone(control);
    let free = free.clone();
    let filled = filled.clone();
    let error_control = Arc::clone(&control);
    let error_tx = errors.clone();
    let channels = (config.channels as usize).max(1);

    device.build_input_stream(
        config.clone(),
        move |data: &[T], _info: &cpal::InputCallbackInfo| {
            if !control.recording.load(Ordering::Relaxed) {
                return;
            }
            let Ok(mut buffer) = free.try_recv() else {
                return;
            };
            buffer.clear();
            for frame in data.chunks(channels) {
                let mut sum = 0.0f32;
                for &sample in frame {
                    sum += f32::from_sample(sample);
                }
                buffer.push(sum / frame.len().max(1) as f32);
            }
            let _ = filled.try_send(buffer);
        },
        move |error| {
            if !is_device_loss(&error) {
                return;
            }
            if error_control.error_sent.swap(true, Ordering::AcqRel) {
                return;
            }
            let _ = error_tx.try_send(error.to_string());
        },
        None,
    )
}

/// Xruns are glitches, not disconnects; the Python recorder ignores overflows too.
fn is_device_loss(error: &cpal::Error) -> bool {
    match error.kind() {
        ErrorKind::Xrun => false,
        ErrorKind::DeviceNotAvailable
        | ErrorKind::HostUnavailable
        | ErrorKind::StreamInvalidated
        | ErrorKind::PermissionDenied => true,
        _ => policy::is_device_error_status(&error.to_string()),
    }
}

fn wav_bytes(samples: &[f32], rate: u32) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    if let Ok(mut writer) = hound::WavWriter::new(&mut cursor, spec) {
        for &sample in samples {
            let _ = writer.write_sample(
                (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16,
            );
        }
        let _ = writer.finalize();
    }
    cursor.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archived_wav_matches_the_capture_rate() {
        let samples = [-1.0f32, 0.0, 1.0, 0.5];
        let bytes = wav_bytes(&samples, 48_000);
        let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, 48_000);
        assert_eq!(spec.bits_per_sample, 16);
        let decoded: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(decoded, vec![-32767, 0, 32767, 16384]);
    }

    #[test]
    fn disconnect_classification_ignores_xruns() {
        assert!(is_device_loss(&cpal::Error::new(
            ErrorKind::DeviceNotAvailable
        )));
        assert!(is_device_loss(&cpal::Error::new(ErrorKind::HostUnavailable)));
        assert!(is_device_loss(&cpal::Error::new(
            ErrorKind::StreamInvalidated
        )));
        assert!(is_device_loss(&cpal::Error::new(
            ErrorKind::PermissionDenied
        )));
        assert!(!is_device_loss(&cpal::Error::new(ErrorKind::Xrun)));
        assert!(!is_device_loss(&cpal::Error::with_message(
            ErrorKind::BackendError,
            "output underflow"
        )));
        assert!(is_device_loss(&cpal::Error::with_message(
            ErrorKind::BackendError,
            "input device unavailable"
        )));
    }

    #[test]
    fn stop_without_a_session_is_a_no_op() {
        let mut capture = CpalCapture::new(String::new(), "system".into(), 120);
        assert_eq!(capture.stop(false), None);
        capture.reset_streaming_state();
        assert_eq!(capture.capture_error(), "");
        assert!(capture.poll_events().is_empty());
    }
}
