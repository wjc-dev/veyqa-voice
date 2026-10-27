use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::Connector;

use super::speech_models::selected_model;
use super::{combine, json_text, json_truthy, parse_event};
use crate::audio::resample::LinearResampler;
use crate::config;
use crate::custom_vocabulary::{configured_path, load_custom_vocabulary};
use crate::insert::corrections::{
    correction_context_enabled, correction_minimum_count, CorrectionStore,
};
use crate::paths;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const FINAL_TIMEOUT: Duration = Duration::from_secs(20);
pub const CONTEXT_CHARACTER_LIMIT: usize = 400;
pub const MAX_INLINE_VOCABULARY_TERMS: usize = 2_000;

pub struct QwenConfig {
    pub api_key: String,
    pub workspace_id: String,
    pub api_host: String,
    pub region: String,
    pub language: String,
    pub model: String,
    pub context_enabled: bool,
    pub recent_memory_enabled: bool,
    pub recent_memory_count: usize,
    pub context_file: PathBuf,
}

impl QwenConfig {
    pub fn from_env() -> Result<Self> {
        let mut api_key = config::var("QWEN_API_KEY");
        if api_key.is_empty() {
            api_key = config::var("DASHSCOPE_API_KEY");
        }
        if api_key.is_empty() {
            bail!("未配置 QWEN_API_KEY / DASHSCOPE_API_KEY");
        }
        Self::with_key(api_key)
    }

    /// Built-in builds use the offline-provisioned trial credential instead of
    /// a personal `.env` key.
    pub fn for_builtin(credential: &super::private_trial::TrialCredential) -> Result<Self> {
        let mut config = Self::with_key(credential.key.clone())?;
        config.api_host = credential.host.clone();
        config.workspace_id = String::new();
        config.region = "beijing".to_string();
        config.model = super::speech_models::promotion_model()
            .map_err(|error| anyhow!(error.to_string()))?;
        Ok(config)
    }

    /// Context/vocabulary options do not depend on the credential.
    pub fn for_context() -> Result<Self> {
        Self::with_key(String::new())
    }

    fn with_key(api_key: String) -> Result<Self> {
        let model = selected_model("qwen", &config::var("QWEN_ASR_MODEL"))
            .map_err(|error| anyhow!(error.to_string()))?;
        let recent_memory_count = std::env::var("QWEN_RECENT_MEMORY_COUNT")
            .unwrap_or_else(|_| "20".to_string())
            .trim()
            .parse::<i64>()
            .unwrap_or(20)
            .clamp(1, 50) as usize;
        let context_file = match std::env::var("QWEN_CONTEXT_FILE") {
            Ok(value) => PathBuf::from(value),
            Err(_) => paths::context_file(),
        };
        Ok(Self {
            api_key,
            workspace_id: config::var("QWEN_WORKSPACE_ID"),
            api_host: config::var("QWEN_API_HOST"),
            region: config::var_or("QWEN_REGION", "beijing").to_lowercase(),
            language: config::var_or("QWEN_LANGUAGE", "zh").to_lowercase(),
            model,
            context_enabled: env_is_true("QWEN_CONTEXT_ENABLED"),
            recent_memory_enabled: env_is_true("QWEN_RECENT_MEMORY_ENABLED"),
            recent_memory_count,
            context_file,
        })
    }

    pub fn build_url(&self) -> Result<String> {
        let host = if !self.api_host.is_empty() {
            let host = hostname(&self.api_host);
            if !host.ends_with(".aliyuncs.com") {
                bail!("千问 API Host 必须是 aliyuncs.com 官方域名");
            }
            host.to_string()
        } else if self.region == "singapore" {
            if !self.workspace_id.is_empty() {
                format!("{}.ap-southeast-1.maas.aliyuncs.com", self.workspace_id)
            } else {
                "dashscope-intl.aliyuncs.com".to_string()
            }
        } else if !self.workspace_id.is_empty() {
            format!("{}.cn-beijing.maas.aliyuncs.com", self.workspace_id)
        } else {
            "dashscope.aliyuncs.com".to_string()
        };
        Ok(format!("wss://{host}/api-ws/v1/inference"))
    }

    pub(crate) fn run_task_event(
        &self,
        task_id: &str,
        sample_rate: u32,
        options: &QwenRequestOptions,
    ) -> Value {
        let mut parameters = json!({
            "format": "pcm",
            "sample_rate": sample_rate,
            "heartbeat": true,
        });
        if self.model.contains("-message") {
            parameters["intermediate_result_enabled"] = json!(true);
            parameters["disfluency_removal_enabled"] = json!(false);
            parameters["keep_dialect"] = json!(true);
        }
        let hints = language_hints(&self.language);
        if !hints.is_empty() {
            parameters["language_hints"] = json!(hints);
        }
        if !options.vocabulary.is_empty() {
            let mut vocabulary = Map::new();
            for (term, weight) in &options.vocabulary {
                vocabulary.insert(term.clone(), json!(weight));
            }
            parameters["vocabulary"] = Value::Object(vocabulary);
        }

        let mut task_input = Map::new();
        if !options.context.is_empty() {
            task_input.insert(
                "context".into(),
                json!([{
                    "role": "user",
                    "content": [{ "type": "input_text", "text": options.context }],
                }]),
            );
        }

        json!({
            "header": {
                "action": "run-task",
                "task_id": task_id,
                "streaming": "duplex",
            },
            "payload": {
                "task_group": "audio",
                "task": "asr",
                "function": "recognition",
                "model": self.model,
                "parameters": parameters,
                "input": Value::Object(task_input),
            }
        })
    }

    /// Exact local context that will bias this ASR session.
    pub fn context_text(&self) -> String {
        self.context_text_with(&CorrectionStore::new(None))
    }

    pub fn context_text_with(&self, store: &CorrectionStore) -> String {
        let mut sections = Vec::new();

        // Confirmed corrections first: Qwen keeps at most 400 characters per
        // context turn, so the most actionable terms must not be pushed out.
        let learned_lines = store.context_lines(None, 100);
        if !learned_lines.is_empty() {
            sections.push(format!(
                "用户反复人工纠正过的语音词汇（优先按右侧形式识别）：\n{}",
                learned_lines.join("\n")
            ));
        }

        if self.context_enabled && self.context_file.exists() {
            if let Ok(text) = fs::read_to_string(&self.context_file) {
                let text = text.trim();
                if !text.is_empty() {
                    sections.push(format!("用户主动配置的背景与词汇：\n{text}"));
                }
            }
        }

        let recent_memory = recent_memory_text(
            &paths::audio_archive_dir().join("cache.json"),
            self.recent_memory_enabled,
            self.recent_memory_count,
        );
        if !recent_memory.is_empty() {
            sections.push(recent_memory);
        }

        compose_context(&sections)
    }

    pub fn request_options(&self) -> QwenRequestOptions {
        let hotwords =
            load_custom_vocabulary(Some(&configured_path()), MAX_INLINE_VOCABULARY_TERMS);
        let rules = if correction_context_enabled() {
            CorrectionStore::new(None).rules()
        } else {
            Vec::new()
        };
        QwenRequestOptions {
            context: self.context_text(),
            vocabulary: inline_vocabulary(&hotwords, &rules, correction_minimum_count()),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct QwenRequestOptions {
    pub context: String,
    pub vocabulary: Vec<(String, u32)>,
}

fn env_is_true(name: &str) -> bool {
    std::env::var(name)
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase()
        == "true"
}

pub fn language_hints(language: &str) -> Vec<String> {
    if language == "zh" {
        // The setting is labelled "Chinese first", not "Chinese only".
        return vec!["zh".into(), "en".into()];
    }
    if !language.is_empty() && language != "auto" {
        return vec![language.to_string()];
    }
    Vec::new()
}

/// Explicit and learned terms as request-level ASR hotwords (`term -> 4`).
pub fn inline_vocabulary(
    hotwords: &[String],
    rules: &[Map<String, Value>],
    minimum_count: i64,
) -> Vec<(String, u32)> {
    let mut vocabulary: Vec<(String, u32)> = Vec::new();
    for term in hotwords {
        if !vocabulary.iter().any(|(existing, _)| existing == term) {
            vocabulary.push((term.clone(), 4));
        }
    }
    for rule in rules {
        let enabled = rule
            .get("enabled")
            .map(|value| json_truthy(Some(value)))
            .unwrap_or(true);
        if !enabled || rule_count(rule) < minimum_count {
            continue;
        }
        let term = json_text(rule.get("correct")).trim().to_string();
        if term.is_empty() || term.chars().count() > 64 {
            continue;
        }
        if !vocabulary.iter().any(|(existing, _)| existing == &term) {
            vocabulary.push((term, 4));
        }
        if vocabulary.len() >= MAX_INLINE_VOCABULARY_TERMS {
            break;
        }
    }
    vocabulary
}

fn rule_count(rule: &Map<String, Value>) -> i64 {
    match rule.get("count") {
        Some(Value::Bool(flag)) => i64::from(*flag),
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|value| value.trunc() as i64))
            .unwrap_or(0),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

pub fn compose_context(sections: &[String]) -> String {
    let text = sections.join("\n\n");
    if text.chars().count() > CONTEXT_CHARACTER_LIMIT {
        text.chars().take(CONTEXT_CHARACTER_LIMIT).collect()
    } else {
        text
    }
}

/// Only this tool's own recent transcripts, and only when explicitly enabled.
pub fn recent_memory_text(cache_path: &Path, enabled: bool, count: usize) -> String {
    if !enabled {
        return String::new();
    }
    let Ok(text) = fs::read_to_string(cache_path) else {
        return String::new();
    };
    let Ok(Value::Object(cache)) = serde_json::from_str::<Value>(&text) else {
        return String::new();
    };

    let mut records: Vec<&Map<String, Value>> = cache.values().filter_map(Value::as_object).collect();
    records.sort_by(|left, right| {
        json_text(right.get("timestamp")).cmp(&json_text(left.get("timestamp")))
    });

    let mut texts: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for record in records {
        let text = json_text(record.get("transcription")).trim().to_string();
        if text.is_empty() || !seen.insert(text.clone()) {
            continue;
        }
        texts.push(text);
        if texts.len() >= count {
            break;
        }
    }
    if texts.is_empty() {
        return String::new();
    }
    format!(
        "近期由本语音工具转写的主题（仅用于识别上下文）：\n{}",
        texts.join("\n")
    )
}

/// Sentence-by-sentence final assembly with sentence-id replacement.
#[derive(Debug, Default)]
pub struct TranscriptAssembler {
    final_segments: Vec<String>,
    finalized_keys: HashSet<String>,
    sentence_indices: HashMap<i64, usize>,
    current_partial: String,
    latest_preview: String,
    task_finished: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QwenEvent {
    Update { preview: String },
    TaskFinished,
    Error(String),
    Ignored,
}

impl TranscriptAssembler {
    pub fn handle(&mut self, event: &Value) -> QwenEvent {
        let header = event.get("header").and_then(Value::as_object);
        let dashscope_event = header
            .and_then(|header| header.get("event"))
            .and_then(Value::as_str)
            .unwrap_or("");

        let update = match dashscope_event {
            "result-generated" => self.handle_result(event),
            "task-failed" => {
                let message = header
                    .and_then(|header| header.get("error_message"))
                    .filter(|value| json_truthy(Some(value)))
                    .or_else(|| {
                        header
                            .and_then(|header| header.get("error_code"))
                            .filter(|value| json_truthy(Some(value)))
                    })
                    .map(|value| json_text(Some(value)))
                    .unwrap_or_else(|| "未知服务端错误".to_string());
                return QwenEvent::Error(format!("千问 ASR 错误: {message}"));
            }
            "task-finished" => None,
            _ => self.handle_legacy(event),
        };

        if let Some(preview) = update {
            if !preview.is_empty() {
                return QwenEvent::Update { preview };
            }
        }
        if header
            .and_then(|header| header.get("event"))
            .and_then(Value::as_str)
            == Some("task-finished")
        {
            self.task_finished = true;
            return QwenEvent::TaskFinished;
        }
        QwenEvent::Ignored
    }

    fn handle_result(&mut self, event: &Value) -> Option<String> {
        let payload = event.get("payload").and_then(Value::as_object);
        let output = payload
            .and_then(|payload| payload.get("output"))
            .and_then(Value::as_object);
        let sentence = output
            .and_then(|output| output.get("sentence"))
            .and_then(Value::as_object);
        if sentence
            .and_then(|sentence| sentence.get("heartbeat"))
            .is_some_and(|heartbeat| json_truthy(Some(heartbeat)))
        {
            return None;
        }
        let text = sentence
            .and_then(|sentence| sentence.get("text"))
            .map(|value| json_text(Some(value)))
            .unwrap_or_default()
            .trim()
            .to_string();
        if text.is_empty() {
            return None;
        }
        let completed = sentence
            .and_then(|sentence| sentence.get("sentence_end"))
            .is_some_and(|value| json_truthy(Some(value)));
        self.apply_text(
            text,
            completed,
            sentence.and_then(|sentence| sentence.get("sentence_id")),
            sentence.and_then(|sentence| sentence.get("begin_time")),
            sentence.and_then(|sentence| sentence.get("end_time")),
        )
    }

    fn handle_legacy(&mut self, event: &Value) -> Option<String> {
        let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "conversation.item.input_audio_transcription.text"
            | "conversation.item.input_audio_transcription.delta" => {
                let mut preview = String::new();
                preview.push_str(&json_text(event.get("text")));
                preview.push_str(&json_text(event.get("stash")));
                let preview = preview.trim().to_string();
                let preview = if preview.is_empty() {
                    json_text(event.get("transcript")).trim().to_string()
                } else {
                    preview
                };
                if preview.is_empty() {
                    None
                } else {
                    self.apply_text(preview, false, None, None, None)
                }
            }
            "conversation.item.input_audio_transcription.completed" => {
                let transcript = json_text(event.get("transcript")).trim().to_string();
                if transcript.is_empty() {
                    None
                } else {
                    self.apply_text(transcript, true, None, None, None)
                }
            }
            _ => None,
        }
    }

    fn apply_text(
        &mut self,
        preview: String,
        completed: bool,
        sentence_id: Option<&Value>,
        begin_time: Option<&Value>,
        end_time: Option<&Value>,
    ) -> Option<String> {
        if completed {
            let key = format!(
                "{}|{}|{}",
                begin_time.map(Value::to_string).unwrap_or_else(|| "null".into()),
                end_time.map(Value::to_string).unwrap_or_else(|| "null".into()),
                preview
            );
            let id = sentence_id
                .and_then(Value::as_i64)
                .filter(|id| *id > 0);
            if let Some(id) = id {
                if let Some(&index) = self.sentence_indices.get(&id) {
                    self.final_segments[index] = preview;
                } else {
                    self.sentence_indices.insert(id, self.final_segments.len());
                    self.final_segments.push(preview);
                }
            } else if self.finalized_keys.insert(key) {
                self.final_segments.push(preview);
            }
            self.current_partial.clear();
        } else {
            self.current_partial = preview;
        }
        let combined = self.combined_text();
        self.latest_preview = combined.clone();
        (!combined.is_empty()).then_some(combined)
    }

    pub fn combined_text(&self) -> String {
        combine(&self.final_segments, &self.current_partial)
    }

    /// `output = combined_text() or latest_preview`.
    pub fn final_text(&self) -> String {
        let combined = self.combined_text();
        if combined.is_empty() {
            self.latest_preview.clone()
        } else {
            combined
        }
    }

    pub fn task_finished(&self) -> bool {
        self.task_finished
    }
}

pub(crate) fn finish_task_event(task_id: &str) -> Value {
    json!({
        "header": {
            "action": "finish-task",
            "task_id": task_id,
            "streaming": "duplex",
        },
        "payload": {"input": {}},
    })
}

pub fn hostname(value: &str) -> &str {
    let rest = value
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(value);
    rest.split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
}

pub async fn probe(path: &Path) -> Result<()> {
    let started = Instant::now();
    let cfg = QwenConfig::from_env()?;
    let (samples, source_rate) = read_wav_mono_16k(path)?;
    if samples.is_empty() {
        bail!("WAV 中没有音频样本");
    }
    let audio_ms = samples.len() as u64 * 1000 / 16_000;
    let pcm = pcm16_bytes(&samples);
    let url = cfg.build_url()?;
    println!(
        "ASR probe: host={} model={} audio={}ms source={}Hz",
        hostname(&url),
        cfg.model,
        audio_ms,
        source_rate
    );

    let mut request = url.as_str().into_client_request()?;
    {
        let headers = request.headers_mut();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", cfg.api_key))?,
        );
        headers.insert(
            "User-Agent",
            HeaderValue::from_static("Veyqa-Voice-Rust/M0"),
        );
        if !cfg.workspace_id.is_empty() && cfg.api_host.is_empty() {
            headers.insert(
                "X-DashScope-WorkSpace",
                HeaderValue::from_str(&cfg.workspace_id)?,
            );
        }
    }

    let connector = Connector::NativeTls(native_tls::TlsConnector::builder().build()?);
    let (mut socket, response) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async_tls_with_config(request, None, true, Some(connector)),
    )
    .await
    .context("连接 DashScope 超时")??;
    if response.status().as_u16() != 101 {
        bail!("WebSocket 握手失败: HTTP {}", response.status());
    }
    let connect_ms = started.elapsed().as_millis();

    let task_id = uuid::Uuid::new_v4().simple().to_string();
    let task_id = &task_id[..32];
    let options = cfg.request_options();
    socket
        .send(Message::Text(
            cfg.run_task_event(task_id, 16_000, &options)
                .to_string()
                .into(),
        ))
        .await?;

    let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
    let mut task_started = false;
    while !task_started {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .context("等待 task-started 超时")?
            .ok_or_else(|| anyhow!("连接在 task-started 前关闭"))??;
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text)?;
            let event = parse_event(&value);
            if let Some(error) = event.error {
                bail!(error);
            }
            if value["header"]["event"].as_str() == Some("task-started") {
                task_started = true;
            }
        }
    }
    let task_started_ms = started.elapsed().as_millis();

    for chunk in pcm.chunks(3_200) {
        socket
            .send(Message::Binary(chunk.to_vec().into()))
            .await?;
    }
    socket
        .send(Message::Text(finish_task_event(task_id).to_string().into()))
        .await?;
    let finish_sent_ms = started.elapsed().as_millis();

    let mut final_segments: Vec<String> = Vec::new();
    let mut sentence_indices: HashMap<i64, usize> = HashMap::new();
    let mut current_partial = String::new();
    let mut partials = 0u32;
    let mut first_partial_ms = None;
    let deadline = tokio::time::Instant::now() + FINAL_TIMEOUT;

    loop {
        let message = match tokio::time::timeout_at(deadline, socket.next()).await {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(error))) => bail!("WebSocket 错误: {error}"),
            Ok(None) => break,
            Err(_) => bail!("等待最终识别结果超时"),
        };
        match message {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text)?;
                let event = parse_event(&value);
                if let Some(error) = event.error {
                    bail!(error);
                }
                if let Some(text) = event.preview {
                    if partials == 0 {
                        first_partial_ms = Some(started.elapsed().as_millis());
                    }
                    partials += 1;
                    if event.completed {
                        match event.sentence_id {
                            Some(id) => match sentence_indices.get(&id) {
                                Some(&index) => final_segments[index] = text,
                                None => {
                                    sentence_indices.insert(id, final_segments.len());
                                    final_segments.push(text);
                                }
                            },
                            None => final_segments.push(text),
                        }
                        current_partial.clear();
                    } else {
                        current_partial = text;
                    }
                    println!(
                        "[+{}ms] {}",
                        started.elapsed().as_millis(),
                        combine(&final_segments, &current_partial)
                    );
                }
                if event.task_finished {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    let final_text = combine(&final_segments, &current_partial);
    println!(
        "{}",
        json!({
            "connect_ms": connect_ms,
            "task_started_ms": task_started_ms,
            "finish_sent_ms": finish_sent_ms,
            "first_partial_ms": first_partial_ms,
            "done_ms": started.elapsed().as_millis(),
            "partials": partials,
            "audio_ms": audio_ms,
            "chars": final_text.chars().count(),
            "final": final_text,
        })
    );
    Ok(())
}

fn read_wav_mono_16k(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("无法打开 WAV: {}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let mut interleaved = Vec::new();
    match spec.sample_format {
        hound::SampleFormat::Int => {
            let scale = if spec.bits_per_sample >= 32 {
                i32::MAX as f32
            } else {
                ((1i64 << spec.bits_per_sample.saturating_sub(1)) - 1) as f32
            };
            for sample in reader.samples::<i32>() {
                interleaved.push(sample? as f32 / scale);
            }
        }
        hound::SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                interleaved.push(sample?);
            }
        }
    }
    let mono = if channels <= 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    let mut resampler = LinearResampler::new(spec.sample_rate, 16_000);
    Ok((resampler.process(&mono), spec.sample_rate))
}

fn pcm16_bytes(samples: &[f32]) -> Vec<u8> {
    let mut output = Vec::with_capacity(samples.len() * 2);
    for &sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        output.extend_from_slice(&value.to_le_bytes());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config(model: &str) -> QwenConfig {
        QwenConfig {
            api_key: "test-key".into(),
            workspace_id: String::new(),
            api_host: String::new(),
            region: "beijing".into(),
            language: "zh".into(),
            model: model.into(),
            context_enabled: false,
            recent_memory_enabled: false,
            recent_memory_count: 20,
            context_file: PathBuf::from("/nonexistent/context.txt"),
        }
    }

    fn rule(wrong: &str, correct: &str, count: i64, enabled: bool) -> Map<String, Value> {
        json!({
            "wrong": wrong,
            "correct": correct,
            "count": count,
            "enabled": enabled,
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn result_event(text: &str, completed: bool, id: Option<i64>) -> Value {
        let mut sentence = json!({ "text": text, "sentence_end": completed });
        if let Some(id) = id {
            sentence["sentence_id"] = json!(id);
        }
        json!({
            "header": { "event": "result-generated" },
            "payload": { "output": { "sentence": sentence } },
        })
    }

    #[test]
    fn builds_region_and_workspace_urls_and_rejects_foreign_hosts() {
        assert_eq!(
            base_config("qwen-audio-3.0-asr-flash-streaming")
                .build_url()
                .unwrap(),
            "wss://dashscope.aliyuncs.com/api-ws/v1/inference"
        );

        let mut config = base_config("qwen-audio-3.0-asr-flash-streaming");
        config.region = "singapore".into();
        assert_eq!(
            config.build_url().unwrap(),
            "wss://dashscope-intl.aliyuncs.com/api-ws/v1/inference"
        );
        config.workspace_id = "ws-1".into();
        assert_eq!(
            config.build_url().unwrap(),
            "wss://ws-1.ap-southeast-1.maas.aliyuncs.com/api-ws/v1/inference"
        );

        let mut config = base_config("qwen-audio-3.0-asr-flash-streaming");
        config.workspace_id = "ws-2".into();
        assert_eq!(
            config.build_url().unwrap(),
            "wss://ws-2.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference"
        );

        config.api_host = "https://dashscope.aliyuncs.com/".into();
        assert_eq!(
            config.build_url().unwrap(),
            "wss://dashscope.aliyuncs.com/api-ws/v1/inference"
        );
        config.api_host = "https://evil.example.com".into();
        assert_eq!(
            config.build_url().unwrap_err().to_string(),
            "千问 API Host 必须是 aliyuncs.com 官方域名"
        );
    }

    #[test]
    fn run_task_carries_pcm_language_context_and_learned_hotwords() {
        let config = base_config("qwen-audio-3.0-asr-flash-streaming");
        let hotwords = vec!["项目代号".to_string(), "LightGBM".to_string()];
        let vocabulary = inline_vocabulary(
            &hotwords,
            &[rule("锤道", "trade off", 2, true)],
            2,
        );
        assert!(vocabulary.contains(&("trade off".into(), 4)));
        assert!(vocabulary.contains(&("项目代号".into(), 4)));
        assert!(vocabulary.contains(&("LightGBM".into(), 4)));

        let long_background = "软件开发，常用 LightGBM、trade-off。".repeat(40);
        let context = compose_context(&[
            "用户反复人工纠正过的语音词汇（优先按右侧形式识别）：\n锤道 → trade off（已人工纠正 2 次）"
                .to_string(),
            format!("用户主动配置的背景与词汇：\n{long_background}"),
        ]);
        assert!(context.chars().count() <= CONTEXT_CHARACTER_LIMIT);
        assert!(context.contains("trade off"));

        let options = QwenRequestOptions {
            context: context.clone(),
            vocabulary,
        };
        let event = config.run_task_event("task-id", 16_000, &options);
        assert_eq!(event["header"]["action"], "run-task");
        assert_eq!(event["header"]["streaming"], "duplex");
        assert_eq!(
            event["payload"]["model"],
            "qwen-audio-3.0-asr-flash-streaming"
        );
        let parameters = &event["payload"]["parameters"];
        assert_eq!(parameters["format"], "pcm");
        assert_eq!(parameters["sample_rate"], 16000);
        assert_eq!(parameters["heartbeat"], true);
        assert_eq!(parameters["language_hints"], json!(["zh", "en"]));
        assert_eq!(parameters["vocabulary"]["trade off"], 4);
        assert_eq!(parameters["vocabulary"]["项目代号"], 4);
        assert_eq!(parameters["vocabulary"]["LightGBM"], 4);
        assert_eq!(
            event["payload"]["input"]["context"][0]["content"][0]["text"],
            context
        );
    }

    #[test]
    fn message_models_enable_intermediate_results_and_auto_language_has_no_hints() {
        let config = base_config("qwen-audio-3.1-asr-flash-message");
        let event = config.run_task_event("task", 16_000, &QwenRequestOptions::default());
        let parameters = &event["payload"]["parameters"];
        assert_eq!(parameters["intermediate_result_enabled"], true);
        assert_eq!(parameters["disfluency_removal_enabled"], false);
        assert_eq!(parameters["keep_dialect"], true);
        assert!(parameters.get("vocabulary").is_none());
        assert_eq!(event["payload"]["input"], json!({}));

        let streaming = base_config("qwen-audio-3.0-asr-flash-streaming");
        let event = streaming.run_task_event("task", 16_000, &QwenRequestOptions::default());
        assert!(event["payload"]["parameters"]
            .get("intermediate_result_enabled")
            .is_none());

        assert!(language_hints("auto").is_empty());
        assert!(language_hints("").is_empty());
        assert_eq!(language_hints("en"), vec!["en".to_string()]);
        assert_eq!(language_hints("zh"), vec!["zh".to_string(), "en".to_string()]);
    }

    #[test]
    fn revised_sentence_replaces_the_previous_final() {
        let mut assembler = TranscriptAssembler::default();
        assert_eq!(
            assembler.handle(&result_event("数据工作", true, Some(1))),
            QwenEvent::Update {
                preview: "数据工作".into()
            }
        );
        assert_eq!(
            assembler.handle(&result_event("DataWorks。", true, Some(1))),
            QwenEvent::Update {
                preview: "DataWorks。".into()
            }
        );
        assert_eq!(
            assembler.handle(&result_event("继续测试。", true, Some(2))),
            QwenEvent::Update {
                preview: "DataWorks。继续测试。".into()
            }
        );
        assert_eq!(
            assembler.handle(&json!({
                "header": { "event": "task-finished" },
                "payload": {},
            })),
            QwenEvent::TaskFinished
        );
        assert!(assembler.task_finished());
        assert_eq!(assembler.final_text(), "DataWorks。继续测试。");
    }

    #[test]
    fn accumulates_finalized_sentences_without_duplicate_output() {
        let mut assembler = TranscriptAssembler::default();
        assembler.handle(&json!({
            "header": { "event": "result-generated" },
            "payload": { "output": { "sentence": {
                "text": "你好", "sentence_end": false, "begin_time": 0
            } } },
        }));
        assembler.handle(&json!({
            "header": { "event": "result-generated" },
            "payload": { "output": { "sentence": {
                "text": "你好。", "sentence_end": true, "begin_time": 0, "end_time": 500
            } } },
        }));
        // Repeating the same finalized boundary must not append twice.
        assembler.handle(&json!({
            "header": { "event": "result-generated" },
            "payload": { "output": { "sentence": {
                "text": "你好。", "sentence_end": true, "begin_time": 0, "end_time": 500
            } } },
        }));
        assembler.handle(&json!({
            "header": { "event": "result-generated" },
            "payload": { "output": { "sentence": {
                "text": "第二句。", "sentence_end": true, "begin_time": 500, "end_time": 1000
            } } },
        }));
        assembler.handle(&json!({
            "header": { "event": "task-finished" },
            "payload": {},
        }));

        assert_eq!(assembler.final_text(), "你好。第二句。");
        assert!(assembler.task_finished());
    }

    #[test]
    fn heartbeat_and_empty_text_are_ignored() {
        let mut assembler = TranscriptAssembler::default();
        assert_eq!(
            assembler.handle(&json!({
                "header": { "event": "result-generated" },
                "payload": { "output": { "sentence": { "heartbeat": true } } },
            })),
            QwenEvent::Ignored
        );
        assert_eq!(
            assembler.handle(&result_event("", false, None)),
            QwenEvent::Ignored
        );
        assert_eq!(assembler.final_text(), "");
    }

    #[test]
    fn task_failed_maps_to_user_facing_error() {
        let mut assembler = TranscriptAssembler::default();
        assert_eq!(
            assembler.handle(&json!({
                "header": { "event": "task-failed", "error_message": "bad request" },
            })),
            QwenEvent::Error("千问 ASR 错误: bad request".into())
        );
        assert_eq!(
            assembler.handle(&json!({
                "header": { "event": "task-failed" },
            })),
            QwenEvent::Error("千问 ASR 错误: 未知服务端错误".into())
        );
    }

    #[test]
    fn legacy_events_still_assemble() {
        let mut assembler = TranscriptAssembler::default();
        assert_eq!(
            assembler.handle(&json!({
                "type": "conversation.item.input_audio_transcription.text",
                "text": "你",
                "stash": "好",
            })),
            QwenEvent::Update {
                preview: "你好".into()
            }
        );
        assert_eq!(
            assembler.handle(&json!({
                "type": "conversation.item.input_audio_transcription.completed",
                "transcript": "你好。",
            })),
            QwenEvent::Update {
                preview: "你好。".into()
            }
        );
        assembler.handle(&json!({
            "type": "conversation.item.input_audio_transcription.completed",
            "transcript": "你好。",
        }));
        assert_eq!(assembler.final_text(), "你好。");
    }

    #[test]
    fn recent_memory_is_opt_in_deduped_and_sorted() {
        let directory = std::env::temp_dir().join(format!(
            "veyqa-recent-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("cache.json");
        std::fs::write(
            &path,
            json!({
                "a.wav": { "transcription": "旧", "timestamp": "2026-10-01T00:00:00.000000" },
                "b.wav": { "transcription": "新二", "timestamp": "2026-10-03T00:00:00.000000" },
                "c.wav": { "transcription": "新一", "timestamp": "2026-10-02T00:00:00.000000" },
                "d.wav": { "transcription": "新一", "timestamp": "2026-10-04T00:00:00.000000" },
                "e.wav": { "transcription": "   ", "timestamp": "2026-10-05T00:00:00.000000" },
                "bad": "not a record",
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(recent_memory_text(&path, false, 2), "");
        assert_eq!(
            recent_memory_text(&path, true, 2),
            "近期由本语音工具转写的主题（仅用于识别上下文）：\n新一\n新二"
        );
        assert_eq!(
            recent_memory_text(&directory.join("missing.json"), true, 2),
            ""
        );
        std::fs::write(&path, "[]").unwrap();
        assert_eq!(recent_memory_text(&path, true, 2), "");
    }

    #[test]
    fn context_is_truncated_by_characters_not_bytes() {
        let section = "汉".repeat(CONTEXT_CHARACTER_LIMIT + 100);
        let context = compose_context(&[section]);
        assert_eq!(context.chars().count(), CONTEXT_CHARACTER_LIMIT);
        assert!(compose_context(&[]).is_empty());
    }

    #[test]
    fn inline_vocabulary_gates_disabled_short_and_long_terms() {
        let hotwords = vec!["LightGBM".to_string()];
        let rules = vec![
            rule("锤道", "trade off", 2, true),
            rule("禁用", "禁用词", 5, false),
            rule("低频", "低频词", 1, true),
            rule("过长", &"长".repeat(65), 5, true),
            rule("空白", "  LightGBM  ", 5, true),
        ];
        let vocabulary = inline_vocabulary(&hotwords, &rules, 2);
        assert_eq!(
            vocabulary,
            vec![
                ("LightGBM".to_string(), 4),
                ("trade off".to_string(), 4)
            ]
        );

        let many: Vec<String> = (0..MAX_INLINE_VOCABULARY_TERMS)
            .map(|index| format!("词{index}"))
            .collect();
        let vocabulary = inline_vocabulary(&many, &rules, 2);
        // Python setdefaults the learned term before checking the cap.
        assert_eq!(vocabulary.len(), MAX_INLINE_VOCABULARY_TERMS + 1);
        assert_eq!(vocabulary.last(), Some(&("trade off".to_string(), 4)));
    }
}
