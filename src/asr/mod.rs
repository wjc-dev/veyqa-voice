pub mod custom;
pub mod custom_provider;
pub mod doubao;
pub mod doubao_provider;
pub mod private_trial;
pub mod qwen;
pub mod qwen_provider;
pub mod qwen_stream;
pub mod speech_models;

use serde_json::Value;

#[derive(Debug, Default)]
pub struct ParsedEvent {
    pub preview: Option<String>,
    pub error: Option<String>,
    pub completed: bool,
    pub sentence_id: Option<i64>,
    pub task_finished: bool,
}

pub fn parse_event(event: &Value) -> ParsedEvent {
    let header = &event["header"];
    let dashscope_event = header["event"].as_str().unwrap_or("");

    match dashscope_event {
        "result-generated" => {
            let sentence = &event["payload"]["output"]["sentence"];
            if sentence["heartbeat"].as_bool() == Some(true) {
                return ParsedEvent::default();
            }
            let text = sentence["text"].as_str().unwrap_or("").trim();
            ParsedEvent {
                preview: (!text.is_empty()).then(|| text.to_string()),
                completed: sentence["sentence_end"].as_bool().unwrap_or(false),
                sentence_id: sentence["sentence_id"].as_i64(),
                ..ParsedEvent::default()
            }
        }
        "task-failed" => ParsedEvent {
            error: header["error_message"]
                .as_str()
                .or_else(|| header["error_code"].as_str())
                .map(|value| format!("千问 ASR 错误: {value}"))
                .or_else(|| Some("千问 ASR 错误: 未知服务端错误".to_string())),
            ..ParsedEvent::default()
        },
        "task-finished" => ParsedEvent {
            task_finished: true,
            ..ParsedEvent::default()
        },
        _ => parse_legacy(event),
    }
}

fn parse_legacy(event: &Value) -> ParsedEvent {
    let event_type = event["type"].as_str().unwrap_or("");
    match event_type {
        "conversation.item.input_audio_transcription.text"
        | "conversation.item.input_audio_transcription.delta" => {
            let mut preview = String::new();
            preview.push_str(event["text"].as_str().unwrap_or(""));
            preview.push_str(event["stash"].as_str().unwrap_or(""));
            let preview = preview.trim();
            if preview.is_empty() {
                let transcript = event["transcript"].as_str().unwrap_or("").trim();
                return ParsedEvent {
                    preview: (!transcript.is_empty()).then(|| transcript.to_string()),
                    ..ParsedEvent::default()
                };
            }
            ParsedEvent {
                preview: Some(preview.to_string()),
                ..ParsedEvent::default()
            }
        }
        "conversation.item.input_audio_transcription.completed" => {
            let transcript = event["transcript"].as_str().unwrap_or("").trim();
            ParsedEvent {
                preview: (!transcript.is_empty()).then(|| transcript.to_string()),
                completed: true,
                ..ParsedEvent::default()
            }
        }
        "conversation.item.input_audio_transcription.failed" | "error" => {
            let message = event["error"]["message"]
                .as_str()
                .or_else(|| event["error"]["code"].as_str())
                .unwrap_or("未知错误");
            ParsedEvent {
                error: Some(format!("千问 ASR 错误: {message}")),
                ..ParsedEvent::default()
            }
        }
        _ => ParsedEvent::default(),
    }
}

pub fn combine(final_segments: &[String], current_partial: &str) -> String {
    let mut text = final_segments.concat();
    text.push_str(current_partial);
    text.trim().to_string()
}

/// Python truthiness for JSON values.
pub(crate) fn json_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|value| value != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

/// Python `str(value or "")`.
pub(crate) fn json_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::Bool(flag)) => {
            if *flag {
                "True".into()
            } else {
                String::new()
            }
        }
        Some(Value::Number(number)) => {
            if number.as_f64() == Some(0.0) {
                String::new()
            } else {
                number.to_string()
            }
        }
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) if items.is_empty() => String::new(),
        Some(Value::Object(map)) if map.is_empty() => String::new(),
        Some(other) => other.to_string(),
    }
}

