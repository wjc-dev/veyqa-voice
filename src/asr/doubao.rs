//! Volcengine Doubao Seed ASR 2.0 binary framing.
//! Mirrors `src/transcription/doubao_streaming.py` without the WebSocket.

use std::borrow::Cow;
use std::io::Write;

use flate2::write::{GzDecoder, GzEncoder};
use flate2::Compression;
use serde_json::{json, Map, Value};

use super::{json_text, json_truthy};
use crate::custom_vocabulary::{configured_path, load_custom_vocabulary};
use crate::insert::corrections::{
    correction_context_enabled, correction_minimum_count, CorrectionStore,
};

pub const DEFAULT_SAMPLE_RATE: u32 = 16_000;
pub const MAX_INLINE_VOCABULARY_TERMS: usize = 1_000;

const CLIENT_FULL_REQUEST: u8 = 0b0001;
const CLIENT_AUDIO_ONLY_REQUEST: u8 = 0b0010;
const SERVER_FULL_RESPONSE: u8 = 0b1001;
const SERVER_ERROR_RESPONSE: u8 = 0b1111;
const FLAG_POS_SEQUENCE: u8 = 0b0001;
const FLAG_NEG_WITH_SEQUENCE: u8 = 0b0011;
const SERIALIZATION_NONE: u8 = 0b0000;
const SERIALIZATION_JSON: u8 = 0b0001;
const COMPRESSION_GZIP: u8 = 0b0001;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StreamingResult {
    pub definite_text: String,
    pub pending_text: String,
    pub is_final: bool,
    pub error: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct RequestSettings {
    pub hotwords: Vec<String>,
    pub correct_words: Vec<(String, String)>,
    pub boosting_table_id: String,
}

/// Per-connection sequence counter for the SAUC binary protocol.
#[derive(Debug, Clone)]
pub struct DoubaoProtocol {
    sequence: i32,
}

impl Default for DoubaoProtocol {
    fn default() -> Self {
        Self { sequence: 1 }
    }
}

impl DoubaoProtocol {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sequence(&self) -> i32 {
        self.sequence
    }

    /// Full request: 4-byte header, positive sequence, gzip length + payload.
    pub fn full_request(&mut self, sample_rate: u32, settings: &RequestSettings) -> Vec<u8> {
        let body = serde_json::to_vec(&request_json(sample_rate, settings))
            .expect("ASR request JSON is always serializable");
        let compressed = gzip_compress(&body);
        let mut packet = Vec::with_capacity(12 + compressed.len());
        packet.extend_from_slice(&header(
            CLIENT_FULL_REQUEST,
            FLAG_POS_SEQUENCE,
            SERIALIZATION_JSON,
        ));
        packet.extend_from_slice(&self.sequence.to_be_bytes());
        packet.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        packet.extend_from_slice(&compressed);
        self.sequence = self.sequence.wrapping_add(1);
        packet
    }

    /// Audio packet; the last one carries a negative sequence and empty audio.
    pub fn audio_request(&mut self, chunk: &[u8], is_last: bool) -> Vec<u8> {
        let (flags, sequence) = if is_last {
            (FLAG_NEG_WITH_SEQUENCE, -self.sequence)
        } else {
            let sequence = self.sequence;
            self.sequence = self.sequence.wrapping_add(1);
            (FLAG_POS_SEQUENCE, sequence)
        };
        let compressed = gzip_compress(chunk);
        let mut packet = Vec::with_capacity(12 + compressed.len());
        packet.extend_from_slice(&header(
            CLIENT_AUDIO_ONLY_REQUEST,
            flags,
            SERIALIZATION_NONE,
        ));
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        packet.extend_from_slice(&compressed);
        packet
    }
}

fn header(message_type: u8, flags: u8, serialization: u8) -> [u8; 4] {
    // Protocol v1, one 32-bit header word, gzip payload.
    [
        (0b0001 << 4) | 1,
        (message_type << 4) | flags,
        (serialization << 4) | COMPRESSION_GZIP,
        0,
    ]
}

fn gzip_compress(data: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(data)
        .expect("writing gzip into a Vec cannot fail");
    encoder.finish().expect("finishing gzip into a Vec cannot fail")
}

fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut decoder = GzDecoder::new(Vec::new());
    decoder.write_all(data)?;
    decoder.finish()
}

fn request_json(sample_rate: u32, settings: &RequestSettings) -> Value {
    let mut request = Map::new();
    request.insert("model_name".into(), json!("bigmodel"));
    request.insert("enable_itn".into(), json!(true));
    request.insert("enable_punc".into(), json!(true));
    request.insert("enable_ddc".into(), json!(true));
    request.insert("show_utterances".into(), json!(true));
    request.insert("result_type".into(), json!("full"));
    // The server performs a second-pass sentence correction after the
    // utterance boundary, which is useful for quiet speech.
    request.insert("enable_nonstream".into(), json!(true));

    let mut context = Map::new();
    if !settings.hotwords.is_empty() {
        let hotwords = settings
            .hotwords
            .iter()
            .map(|term| json!({ "word": term }))
            .collect();
        context.insert("hotwords".into(), Value::Array(hotwords));
    }
    if !settings.correct_words.is_empty() {
        let mut correct_words = Map::new();
        for (alias, term) in &settings.correct_words {
            correct_words.insert(alias.clone(), Value::String(term.clone()));
        }
        context.insert("correct_words".into(), Value::Object(correct_words));
    }
    if !context.is_empty() {
        request.insert(
            "context".into(),
            Value::String(
                serde_json::to_string(&Value::Object(context))
                    .expect("context JSON is always serializable"),
            ),
        );
    }

    // A provisioned Volcengine hotword table has the most predictable effect.
    if !settings.boosting_table_id.is_empty() {
        request.insert(
            "corpus".into(),
            json!({ "boosting_table_id": settings.boosting_table_id }),
        );
    }

    json!({
        "user": { "uid": "voice_input_next" },
        "audio": {
            "format": "pcm",
            "codec": "raw",
            "rate": sample_rate,
            "bits": 16,
            "channel": 1,
        },
        "request": Value::Object(request),
    })
}

/// Decode one binary server frame into preview/final text or an error.
pub fn parse_response(message: &[u8]) -> StreamingResult {
    if message.len() < 4 {
        return StreamingResult {
            error: Some("豆包响应过短".into()),
            ..StreamingResult::default()
        };
    }
    let header_size = usize::from(message[0] & 0x0F);
    let message_type = message[1] >> 4;
    let flags = message[1] & 0x0F;
    let serialization = message[2] >> 4;
    let compression = message[2] & 0x0F;

    let mut payload = message.get(header_size * 4..).unwrap_or(&[]);
    if flags & FLAG_POS_SEQUENCE != 0 {
        if payload.len() < 4 {
            return StreamingResult {
                error: Some("豆包响应缺少序号".into()),
                ..StreamingResult::default()
            };
        }
        payload = &payload[4..];
    }
    let is_final = flags & 0b0010 != 0;

    if message_type == SERVER_ERROR_RESPONSE {
        if payload.len() < 8 {
            return StreamingResult {
                error: Some("豆包返回未知服务器错误".into()),
                is_final: true,
                ..StreamingResult::default()
            };
        }
        let code = i32::from_be_bytes(payload[..4].try_into().expect("four bytes"));
        let body = &payload[8..];
        let body: Cow<'_, [u8]> =
            if compression == COMPRESSION_GZIP && !body.is_empty() {
                match gzip_decompress(body) {
                    Ok(data) => Cow::Owned(data),
                    Err(_) => Cow::Borrowed(body),
                }
            } else {
                Cow::Borrowed(body)
            };
        let text = String::from_utf8_lossy(&body);
        let clipped: String = text.chars().take(300).collect();
        return StreamingResult {
            error: Some(format!("豆包服务器错误 {code}: {clipped}")),
            is_final: true,
            ..StreamingResult::default()
        };
    }

    if message_type == SERVER_FULL_RESPONSE {
        if payload.len() < 4 {
            return StreamingResult {
                error: Some("豆包响应缺少载荷长度".into()),
                ..StreamingResult::default()
            };
        }
        payload = &payload[4..];
    }
    if payload.is_empty() {
        return StreamingResult {
            is_final,
            ..StreamingResult::default()
        };
    }

    let decompressed;
    let payload: &[u8] = if compression == COMPRESSION_GZIP {
        match gzip_decompress(payload) {
            Ok(data) => {
                decompressed = data;
                &decompressed
            }
            Err(error) => {
                return StreamingResult {
                    error: Some(format!("豆包响应解压失败: {error}")),
                    ..StreamingResult::default()
                };
            }
        }
    } else {
        payload
    };
    if serialization != SERIALIZATION_JSON {
        return StreamingResult {
            is_final,
            ..StreamingResult::default()
        };
    }
    let data: Value = match serde_json::from_slice(payload) {
        Ok(data) => data,
        Err(error) => {
            return StreamingResult {
                error: Some(format!("豆包响应解析失败: {error}")),
                ..StreamingResult::default()
            };
        }
    };
    let mut result = text_result(&data);
    result.is_final = is_final;
    result
}

fn text_result(data: &Value) -> StreamingResult {
    let Some(response) = data.get("result").and_then(Value::as_object) else {
        return StreamingResult::default();
    };
    if let Some(utterances) = response.get("utterances").and_then(Value::as_array) {
        if !utterances.is_empty() {
            let mut definite = String::new();
            let mut pending = String::new();
            for utterance in utterances {
                let Some(utterance) = utterance.as_object() else {
                    continue;
                };
                let text = json_text(utterance.get("text"));
                if json_truthy(utterance.get("definite")) {
                    definite.push_str(&text);
                } else {
                    pending.push_str(&text);
                }
            }
            return StreamingResult {
                definite_text: definite,
                pending_text: pending,
                ..StreamingResult::default()
            };
        }
    }
    StreamingResult {
        pending_text: json_text(response.get("text")),
        ..StreamingResult::default()
    }
}

fn python_int(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Bool(flag) => Some(i64::from(*flag)),
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|value| value.trunc() as i64)),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// Learned corrections first, then safe aliases for Latin hotwords.
/// Aliases only fill gaps (`setdefault`), so a learned pair always wins.
pub fn learned_correct_words(
    hotwords: &[String],
    rules: &[Map<String, Value>],
    minimum_count: i64,
) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for rule in rules {
        if !json_truthy(rule.get("enabled")) && rule.contains_key("enabled") {
            continue;
        }
        if python_int(rule.get("count")).unwrap_or(0) < minimum_count {
            continue;
        }
        let wrong = json_text(rule.get("wrong")).trim().to_string();
        let correct = json_text(rule.get("correct")).trim().to_string();
        if wrong.is_empty() || correct.is_empty() || wrong == correct {
            continue;
        }
        match pairs.iter_mut().find(|(alias, _)| alias == &wrong) {
            Some(pair) => pair.1 = correct,
            None => pairs.push((wrong, correct)),
        }
    }

    for term in hotwords {
        if !term.chars().any(|character| character.is_ascii_alphabetic()) {
            continue;
        }
        let mut aliases = vec![
            term.to_lowercase(),
            replace_runs(term, |character| character == '-' || character == '_', " ")
                .to_lowercase(),
            remove_runs(term, |character| {
                character == '-' || character == '_' || character.is_whitespace()
            })
            .to_lowercase(),
        ];
        aliases.dedup();
        for alias in aliases {
            let alias = alias.trim().to_string();
            if alias.is_empty() || alias == *term {
                continue;
            }
            if !pairs.iter().any(|(existing, _)| existing == &alias) {
                pairs.push((alias, term.clone()));
            }
        }
    }
    pairs
}

fn replace_runs<F: Fn(char) -> bool>(text: &str, separator: F, replacement: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut in_run = false;
    for character in text.chars() {
        if separator(character) {
            if !in_run {
                output.push_str(replacement);
                in_run = true;
            }
        } else {
            output.push(character);
            in_run = false;
        }
    }
    output
}

fn remove_runs<F: Fn(char) -> bool>(text: &str, predicate: F) -> String {
    text.chars().filter(|character| !predicate(*character)).collect()
}

/// Assemble hotwords, learned corrections, and aliases for one utterance.
pub fn request_settings(hotwords: &[String]) -> RequestSettings {
    let correct_words = if correction_context_enabled() {
        let store = CorrectionStore::new(None);
        learned_correct_words(hotwords, &store.rules(), correction_minimum_count())
    } else {
        learned_correct_words(hotwords, &[], 2)
    };
    RequestSettings {
        hotwords: hotwords.to_vec(),
        correct_words,
        boosting_table_id: std::env::var("DOUBAO_BOOSTING_TABLE_ID")
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

pub fn request_settings_from_environment() -> RequestSettings {
    let hotwords = load_custom_vocabulary(
        Some(&configured_path()),
        MAX_INLINE_VOCABULARY_TERMS,
    );
    request_settings(&hotwords)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn decode_full_request(packet: &[u8]) -> Value {
        assert_eq!(&packet[..4], &[0x11, 0x11, 0x11, 0x00]);
        assert_eq!(i32::from_be_bytes(packet[4..8].try_into().unwrap()), 1);
        let size = u32::from_be_bytes(packet[8..12].try_into().unwrap()) as usize;
        assert_eq!(size, packet.len() - 12);
        let body = gzip_decompress(&packet[12..]).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn response(message_type: u8, flags: u8, serialization: u8, body: &[u8]) -> Vec<u8> {
        let mut packet = header(message_type, flags, serialization).to_vec();
        if flags & FLAG_POS_SEQUENCE != 0 {
            packet.extend_from_slice(&7i32.to_be_bytes());
        }
        if message_type == SERVER_FULL_RESPONSE {
            packet.extend_from_slice(&(body.len() as u32).to_be_bytes());
        }
        packet.extend_from_slice(body);
        packet
    }

    #[test]
    fn full_request_contract_matches_python() {
        let hotwords = vec!["项目代号".to_string(), "LightGBM".to_string()];
        let settings = RequestSettings {
            correct_words: learned_correct_words(
                &hotwords,
                &[rule("错误写法", "正确写法", 2, true)],
                2,
            ),
            hotwords,
            boosting_table_id: "hotword-table-123".into(),
        };
        let mut protocol = DoubaoProtocol::new();
        let packet = protocol.full_request(DEFAULT_SAMPLE_RATE, &settings);
        assert_eq!(protocol.sequence(), 2);

        let payload = decode_full_request(&packet);
        assert_eq!(payload["audio"]["rate"], 16000);
        assert_eq!(payload["audio"]["bits"], 16);
        assert_eq!(payload["audio"]["channel"], 1);
        assert_eq!(payload["request"]["model_name"], "bigmodel");
        assert_eq!(payload["request"]["result_type"], "full");
        assert_eq!(payload["request"]["enable_ddc"], true);
        assert_eq!(payload["request"]["enable_nonstream"], true);

        let context: Value =
            serde_json::from_str(payload["request"]["context"].as_str().unwrap()).unwrap();
        assert_eq!(
            context["hotwords"],
            json!([{ "word": "项目代号" }, { "word": "LightGBM" }])
        );
        assert_eq!(context["correct_words"]["lightgbm"], "LightGBM");
        assert_eq!(context["correct_words"]["错误写法"], "正确写法");
        assert_eq!(
            payload["request"]["corpus"],
            json!({ "boosting_table_id": "hotword-table-123" })
        );
    }

    #[test]
    fn empty_settings_omit_context_and_corpus() {
        let mut protocol = DoubaoProtocol::new();
        let packet = protocol.full_request(
            DEFAULT_SAMPLE_RATE,
            &RequestSettings::default(),
        );
        let payload = decode_full_request(&packet);
        assert!(payload["request"].get("context").is_none());
        assert!(payload["request"].get("corpus").is_none());
    }

    #[test]
    fn parses_definite_pending_and_final() {
        let data = json!({
            "result": {
                "text": "完整结果",
                "utterances": [
                    { "text": "已经", "definite": true },
                    { "text": "确定", "definite": false },
                ]
            }
        });
        let compressed = gzip_compress(data.to_string().as_bytes());
        let packet = response(SERVER_FULL_RESPONSE, 0b0011, SERIALIZATION_JSON, &compressed);
        let result = parse_response(&packet);
        assert_eq!(result.definite_text, "已经");
        assert_eq!(result.pending_text, "确定");
        assert!(result.is_final);
        assert!(result.error.is_none());
    }

    #[test]
    fn falls_back_to_result_text_without_utterances() {
        let data = json!({ "result": { "text": "完整结果" } });
        let compressed = gzip_compress(data.to_string().as_bytes());
        let result = parse_response(&response(
            SERVER_FULL_RESPONSE,
            0,
            SERIALIZATION_JSON,
            &compressed,
        ));
        assert_eq!(result.definite_text, "");
        assert_eq!(result.pending_text, "完整结果");
        assert!(!result.is_final);
    }

    #[test]
    fn ignores_non_object_utterances_and_python_falsy_text() {
        let data = json!({
            "result": {
                "utterances": [
                    "bad",
                    { "text": "", "definite": true },
                    { "text": "好的", "definite": true },
                    { "text": 0, "definite": false },
                    { "text": "继续", "definite": false },
                ]
            }
        });
        let compressed = gzip_compress(data.to_string().as_bytes());
        let result = parse_response(&response(
            SERVER_FULL_RESPONSE,
            0,
            SERIALIZATION_JSON,
            &compressed,
        ));
        assert_eq!(result.definite_text, "好的");
        assert_eq!(result.pending_text, "继续");
    }

    #[test]
    fn audio_framing_uses_positive_sequence_then_negative_terminal() {
        let mut protocol = DoubaoProtocol::new();
        protocol.full_request(DEFAULT_SAMPLE_RATE, &RequestSettings::default());

        let first = protocol.audio_request(b"abc", false);
        assert_eq!(first[0], 0x11);
        assert_eq!(first[1], 0x21);
        assert_eq!(first[2], 0x01);
        assert_eq!(i32::from_be_bytes(first[4..8].try_into().unwrap()), 2);
        assert_eq!(gzip_decompress(&first[12..]).unwrap(), b"abc");

        let second = protocol.audio_request(b"def", false);
        assert_eq!(i32::from_be_bytes(second[4..8].try_into().unwrap()), 3);

        let last = protocol.audio_request(b"", true);
        assert_eq!(last[1], 0x23);
        assert_eq!(i32::from_be_bytes(last[4..8].try_into().unwrap()), -4);
        assert!(gzip_decompress(&last[12..]).unwrap().is_empty());
        assert_eq!(protocol.sequence(), 4);
    }

    #[test]
    fn server_error_reports_code_and_message_and_is_final() {
        let mut body = 40_101i32.to_be_bytes().to_vec();
        body.extend_from_slice(&0i32.to_be_bytes());
        body.extend_from_slice(&gzip_compress(b"invalid token"));
        let result = parse_response(&response(SERVER_ERROR_RESPONSE, 0, SERIALIZATION_NONE, &body));
        assert!(result.is_final);
        assert_eq!(
            result.error.as_deref(),
            Some("豆包服务器错误 40101: invalid token")
        );
    }

    #[test]
    fn truncates_long_server_error_messages_to_300_chars() {
        let message = "错".repeat(400);
        let mut body = 5_000i32.to_be_bytes().to_vec();
        body.extend_from_slice(&0i32.to_be_bytes());
        body.extend_from_slice(&gzip_compress(message.as_bytes()));
        let result = parse_response(&response(SERVER_ERROR_RESPONSE, 0, SERIALIZATION_NONE, &body));
        let error = result.error.unwrap();
        assert!(error.starts_with("豆包服务器错误 5000: "));
        assert_eq!(error.chars().count(), "豆包服务器错误 5000: ".chars().count() + 300);
    }

    #[test]
    fn short_and_malformed_frames_surface_errors() {
        assert_eq!(parse_response(&[0x11, 0x90]).error.as_deref(), Some("豆包响应过短"));
        assert_eq!(
            parse_response(&[0x11, 0x91, 0x01, 0x00])
                .error
                .as_deref(),
            Some("豆包响应缺少序号")
        );
        assert_eq!(
            parse_response(&[0x11, 0x91, 0x01, 0x00, 0, 0, 0, 7])
                .error
                .as_deref(),
            Some("豆包响应缺少载荷长度")
        );
        assert_eq!(
            parse_response(&[0x11, 0xF0, 0x01, 0x00, 0, 0, 0, 1])
                .error
                .as_deref(),
            Some("豆包返回未知服务器错误")
        );
    }

    #[test]
    fn corrupt_gzip_and_corrupt_json_report_errors() {
        let result = parse_response(&response(
            SERVER_FULL_RESPONSE,
            0,
            SERIALIZATION_JSON,
            b"not gzip",
        ));
        assert!(result.error.unwrap().starts_with("豆包响应解压失败: "));

        let result = parse_response(&response(
            SERVER_FULL_RESPONSE,
            0,
            SERIALIZATION_JSON,
            &gzip_compress(b"not json"),
        ));
        assert!(result.error.unwrap().starts_with("豆包响应解析失败: "));
    }

    #[test]
    fn non_json_serialization_yields_final_marker_only() {
        let result = parse_response(&response(
            SERVER_FULL_RESPONSE,
            0b0010,
            SERIALIZATION_NONE,
            &gzip_compress(b"whatever"),
        ));
        assert!(result.is_final);
        assert!(result.error.is_none());
        assert_eq!(result.definite_text, "");
        assert_eq!(result.pending_text, "");
    }

    #[test]
    fn learned_rules_respect_gates_and_last_write_wins() {
        let rules = vec![
            rule("错误写法", "正确写法", 1, true),
            rule("被禁用", "禁用后", 5, false),
            rule("同词", "同词", 5, true),
            rule("重复", "第一次", 2, true),
            rule("重复", "第二次", 3, true),
        ];
        let pairs = learned_correct_words(&[], &rules, 2);
        assert_eq!(
            pairs,
            vec![("重复".to_string(), "第二次".to_string())]
        );
    }

    #[test]
    fn latin_hotword_aliases_fill_gaps_but_never_override_learned_rules() {
        let hotwords = vec!["LightGBM".to_string(), "Trade Off".to_string()];
        let rules = vec![rule("lightgbm", "LightGBM-Old", 2, true)];
        let pairs = learned_correct_words(&hotwords, &rules, 2);
        assert_eq!(pairs[0], ("lightgbm".into(), "LightGBM-Old".into()));
        assert!(pairs.contains(&("tradeoff".into(), "Trade Off".into())));
        assert_eq!(
            pairs.iter().filter(|(alias, _)| alias == "lightgbm").count(),
            1
        );

        let separated = learned_correct_words(&["Light-GBM".to_string()], &[], 2);
        assert!(separated.contains(&("light-gbm".into(), "Light-GBM".into())));
        assert!(separated.contains(&("light gbm".into(), "Light-GBM".into())));
        assert!(separated.contains(&("lightgbm".into(), "Light-GBM".into())));
    }

    #[test]
    fn enabled_missing_counts_as_true_like_python() {
        let mut rule = rule("甲", "乙", 2, true);
        rule.remove("enabled");
        let pairs = learned_correct_words(&[], &[rule], 2);
        assert_eq!(pairs, vec![("甲".to_string(), "乙".to_string())]);
    }
}
