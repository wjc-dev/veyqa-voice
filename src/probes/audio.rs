use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use serde_json::json;

use crate::audio::devices::{describe, select_input_device};
use crate::audio::resample::LinearResampler;
use crate::config;

const TARGET_RATE: u32 = 16_000;

#[derive(Default)]
struct CaptureState {
    samples: Vec<f32>,
    callback_times: Vec<u64>,
    peak: f32,
}

struct Capture {
    state: Arc<Mutex<CaptureState>>,
    first_callback_ms: Arc<AtomicU64>,
    callbacks: Arc<AtomicUsize>,
    errors: Arc<Mutex<Vec<String>>>,
}

fn choose_config(device: &cpal::Device) -> Result<(StreamConfig, SampleFormat)> {
    if let Ok(ranges) = device.supported_input_configs() {
        for range in ranges {
            if range.min_sample_rate() <= TARGET_RATE && TARGET_RATE <= range.max_sample_rate() {
                return Ok((
                    StreamConfig {
                        channels: range.channels(),
                        sample_rate: TARGET_RATE,
                        buffer_size: cpal::BufferSize::Default,
                    },
                    range.sample_format(),
                ));
            }
        }
    }
    let supported = device
        .default_input_config()
        .context("无法读取默认输入配置")?;
    let format = supported.sample_format();
    Ok((supported.config(), format))
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    capture: &Capture,
    started: Instant,
) -> Result<cpal::Stream>
where
    T: SizedSample + 'static,
    f32: FromSample<T>,
{
    let state = Arc::clone(&capture.state);
    let first = Arc::clone(&capture.first_callback_ms);
    let callbacks = Arc::clone(&capture.callbacks);
    let errors = Arc::clone(&capture.errors);
    let channels = config.channels as usize;

    let stream = device.build_input_stream(
        config.clone(),
        move |data: &[T], _info: &cpal::InputCallbackInfo| {
            let now = Instant::now();
            let elapsed_ms = now.duration_since(started).as_millis() as u64;
            first.fetch_min(elapsed_ms, Ordering::Relaxed);
            callbacks.fetch_add(1, Ordering::Relaxed);
            let mut guard = match state.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.callback_times.push(elapsed_ms);
            for frame in data.chunks(channels.max(1)) {
                let mut sum = 0.0f32;
                for &sample in frame {
                    sum += f32::from_sample(sample);
                }
                let mono = sum / frame.len() as f32;
                guard.peak = guard.peak.max(mono.abs());
                guard.samples.push(mono);
            }
        },
        move |err| {
            if let Ok(mut errors) = errors.lock() {
                if errors.len() < 8 {
                    errors.push(err.to_string());
                }
            }
        },
        None,
    )?;
    Ok(stream)
}

fn write_wav(path: &Path, samples: &[f32], rate: u32) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("无法写入 {}", path.display()))?;
    for &sample in samples {
        writer.write_sample((sample.clamp(-1.0, 1.0) * 32767.0).round() as i16)?;
    }
    writer.finalize()?;
    Ok(())
}

fn frame_rms(samples: &[f32], frame_len: usize) -> Vec<f64> {
    samples
        .chunks(frame_len.max(1))
        .filter(|frame| !frame.is_empty())
        .map(|frame| {
            let sum: f64 = frame.iter().map(|&v| (v as f64) * (v as f64)).sum();
            (sum / frame.len() as f64).sqrt() * 32767.0
        })
        .collect()
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

pub fn run(
    seconds: f32,
    output: Option<PathBuf>,
    device_override: Option<String>,
    buffer_frames: Option<u32>,
) -> Result<()> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.description().ok().map(|d| d.name().to_string()))
        .unwrap_or_default();

    println!("=== 输入设备 ===");
    if let Ok(devices) = host.input_devices() {
        for (index, device) in devices.enumerate() {
            let name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "未知设备".to_string());
            let mark = if name == default_name { " [系统默认]" } else { "" };
            println!("#{index} {name}{mark} — {}", describe(&device));
        }
    }

    let select_started = Instant::now();
    let configured = device_override.unwrap_or_else(|| config::var("AUDIO_INPUT_DEVICE"));
    let policy = config::var_or("AUDIO_DEVICE_POLICY", "system");
    let (name, device) = select_input_device(&host, &configured, &policy)?;
    let (mut stream_config, sample_format) = choose_config(&device)?;
    if let Some(frames) = buffer_frames {
        stream_config.buffer_size = cpal::BufferSize::Fixed(frames);
    }
    let select_ms = select_started.elapsed().as_millis() as u64;
    let rate = stream_config.sample_rate;
    println!();
    println!("选择设备: {name}");
    println!(
        "采集配置: {}Hz / {}ch / {:?}（目标 {}Hz）",
        rate, stream_config.channels, sample_format, TARGET_RATE
    );

    let capture = Capture {
        state: Arc::new(Mutex::new(CaptureState::default())),
        first_callback_ms: Arc::new(AtomicU64::new(u64::MAX)),
        callbacks: Arc::new(AtomicUsize::new(0)),
        errors: Arc::new(Mutex::new(Vec::new())),
    };

    let started = Instant::now();
    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(&device, &stream_config, &capture, started)?,
        SampleFormat::I16 => build_stream::<i16>(&device, &stream_config, &capture, started)?,
        SampleFormat::U16 => build_stream::<u16>(&device, &stream_config, &capture, started)?,
        SampleFormat::I32 => build_stream::<i32>(&device, &stream_config, &capture, started)?,
        other => bail!("暂不支持的采样格式: {other:?}"),
    };
    let build_ms = started.elapsed().as_millis() as u64;
    stream.play().context("无法启动音频流（检查麦克风权限）")?;
    let play_ms = started.elapsed().as_millis() as u64;
    println!("录音 {seconds:.1}s ...");
    std::thread::sleep(Duration::from_secs_f32(seconds.max(0.1)));
    let stream_started_ms = started.elapsed().as_millis() as u64;
    drop(stream);

    let (samples, callback_times, peak) = {
        let mut guard = capture
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capture state poisoned"))?;
        (
            std::mem::take(&mut guard.samples),
            std::mem::take(&mut guard.callback_times),
            guard.peak,
        )
    };
    let errors = capture
        .errors
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    let first_ms = capture.first_callback_ms.load(Ordering::Relaxed);
    let callbacks = capture.callbacks.load(Ordering::Relaxed);
    let first_ms = if first_ms == u64::MAX { None } else { Some(first_ms) };

    if samples.is_empty() {
        println!();
        println!("未收到任何音频回调。");
        println!("macOS 首次运行需要“麦克风”授权；终端运行时授权对象是终端。");
        for error in &errors {
            println!("stream error: {error}");
        }
        bail!("没有采集到音频");
    }

    let max_gap = callback_times
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]))
        .max()
        .unwrap_or(0);

    let threshold = config::f32_var("VOICE_RMS_THRESHOLD", 60.0).max(20.0);
    let frames = frame_rms(&samples, (rate / 10) as usize);
    let frame_ms = 1000.0 / 10.0;
    let voiced_ms = frames.iter().filter(|&&rms| rms >= threshold as f64).count() as f64 * frame_ms;
    let total_ms = frames.len() as f64 * frame_ms;
    let mut sorted = frames.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let resampled = if rate == TARGET_RATE {
        samples.clone()
    } else {
        LinearResampler::new(rate, TARGET_RATE).process(&samples)
    };
    let out_path = output.unwrap_or_else(|| PathBuf::from("/tmp/veyqa-audio-probe.wav"));
    write_wav(&out_path, &samples, rate)?;
    let resampled_path = out_path.with_extension(format!("{}k.wav", TARGET_RATE / 1000));
    write_wav(&resampled_path, &resampled, TARGET_RATE)?;

    let summary = json!({
        "device": name,
        "rate": rate,
        "channels": stream_config.channels,
        "sample_format": format!("{sample_format:?}"),
        "requested_seconds": seconds,
        "select_ms": select_ms,
        "build_ms": build_ms,
        "play_return_ms": play_ms,
        "stream_alive_ms": stream_started_ms,
        "first_callback_ms": first_ms,
        "callbacks": callbacks,
        "max_callback_gap_ms": max_gap,
        "captured_ms": (samples.len() as u64 * 1000 / rate as u64),
        "peak": peak,
        "rms_threshold": threshold,
        "rms_p50": percentile(&sorted, 0.50),
        "rms_p90": percentile(&sorted, 0.90),
        "voiced_ms": voiced_ms,
        "voiced_ratio": if total_ms > 0.0 { voiced_ms / total_ms } else { 0.0 },
        "wav": out_path.display().to_string(),
        "wav_16k": resampled_path.display().to_string(),
        "errors": errors,
    });
    println!();
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
