//! OpenAI-compatible transcription submitted when recording ends.
//!
//! Mirrors `CustomASRProcessor.process_audio_stream`: buffer the whole
//! utterance, wrap it in a 16 kHz WAV, POST one multipart request without
//! following redirects, and require a `text` field in the JSON reply.

use std::time::Duration;

use crossbeam_channel::Sender;
use serde_json::Value;
use tokio::sync::mpsc;
use url::Url;

use super::custom::{
    transcription_url, wav_bytes, CustomAsrSettings, MAX_AUDIO_BYTES, MAX_RESPONSE_BYTES,
};
use crate::engine::{ProviderEvent, ProviderStream};
use crate::http::{self, Limits};

const SAMPLE_RATE: u32 = 16_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Run one utterance. Always emits `Finished`, exactly once, after any outcome.
pub async fn run_custom(
    stream: ProviderStream,
    settings: CustomAsrSettings,
    events: Sender<ProviderEvent>,
) {
    let session_id = stream.session_id.clone();
    let mut audio = stream.audio;
    let result = transcribe(&settings, &mut audio).await;
    match result {
        Ok(Some(text)) => {
            send(
                &events,
                ProviderEvent::Final {
                    session_id: session_id.clone(),
                    text,
                },
            );
            send(
                &events,
                ProviderEvent::Complete {
                    session_id: session_id.clone(),
                },
            );
        }
        Ok(None) => send(
            &events,
            ProviderEvent::Complete {
                session_id: session_id.clone(),
            },
        ),
        Err(error) => send(
            &events,
            ProviderEvent::Error {
                session_id: session_id.clone(),
                message: error,
            },
        ),
    }
    send(&events, ProviderEvent::Finished { session_id });
}

async fn transcribe(
    settings: &CustomAsrSettings,
    audio: &mut mpsc::Receiver<Vec<u8>>,
) -> Result<Option<String>, String> {
    if !settings.is_available() {
        return Err("请检查自定义服务的地址、API Key 和模型".to_string());
    }
    let endpoint = transcription_url(&settings.base_url)
        .map_err(|error| error.to_string())?;
    let url = Url::parse(&endpoint).map_err(|_| "请检查自定义服务的地址、API Key 和模型")?;

    let mut pcm = Vec::new();
    while let Some(chunk) = audio.recv().await {
        if pcm.len() + chunk.len() > MAX_AUDIO_BYTES {
            return Err("本次录音过长，请分段输入".to_string());
        }
        pcm.extend_from_slice(&chunk);
    }
    if pcm.is_empty() {
        return Ok(None);
    }
    if pcm.len() % 2 != 0 {
        return Err("录音格式异常，请重新录制".to_string());
    }

    let (content_type, body) = multipart_body(&pcm, &settings.model_name);
    let authorization = if settings.api_key.is_empty() {
        None
    } else {
        Some(format!("Bearer {}", settings.api_key))
    };
    let headers: Vec<(&str, &str)> = authorization
        .as_deref()
        .map(|value| vec![("Authorization", value)])
        .unwrap_or_default();

    let response = http::post(
        &url,
        &headers,
        &content_type,
        &body,
        Limits {
            connect: CONNECT_TIMEOUT,
            total: REQUEST_TIMEOUT,
            max_body: MAX_RESPONSE_BYTES,
        },
    )
    .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            let text = format!("{error:#}");
            return Err(if text.contains("响应内容过大") {
                "自定义服务返回内容过大".to_string()
            } else {
                "自定义服务连接失败或超时，请检查网络和 API 地址".to_string()
            });
        }
    };

    if response.status != 200 {
        return Err(if matches!(response.status, 401 | 403) {
            "自定义服务鉴权失败，请检查 API Key 和模型权限".to_string()
        } else {
            format!(
                "自定义服务请求失败（HTTP {}），请检查地址和模型",
                response.status
            )
        });
    }

    let data: Value = serde_json::from_slice(&response.body)
        .map_err(|_| "自定义服务未返回有效的转写结果".to_string())?;
    let Some(text) = data.get("text").and_then(Value::as_str) else {
        return Err("自定义服务响应缺少 text 字段".to_string());
    };
    let text = text.trim();
    Ok((!text.is_empty()).then(|| text.to_string()))
}

fn multipart_body(pcm: &[u8], model: &str) -> (String, Vec<u8>) {
    let boundary = format!("----veyqa{}", uuid::Uuid::new_v4().simple());
    let wav = wav_bytes(pcm, SAMPLE_RATE);
    let mut body = Vec::with_capacity(wav.len() + 512);
    write_field(
        &mut body,
        &boundary,
        "file",
        Some(("speech.wav", "audio/wav")),
        &wav,
    );
    write_field(&mut body, &boundary, "model", None, model.as_bytes());
    write_field(&mut body, &boundary, "response_format", None, b"json");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

fn write_field(
    body: &mut Vec<u8>,
    boundary: &str,
    name: &str,
    file: Option<(&str, &str)>,
    value: &[u8],
) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    match file {
        Some((filename, content_type)) => body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
            )
            .as_bytes(),
        ),
        None => body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        ),
    }
    body.extend_from_slice(value);
    body.extend_from_slice(b"\r\n");
}

fn send(events: &Sender<ProviderEvent>, event: ProviderEvent) {
    let _ = events.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn serve_once(
        response: &'static [u8],
    ) -> (String, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut captured = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                captured.extend_from_slice(&buffer[..read]);
                if let Some(end) = captured.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&captured[..end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if captured.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let _ = sender.send(captured);
            socket.write_all(response).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        (format!("http://{address}/v1"), receiver)
    }

    fn settings(base_url: String) -> CustomAsrSettings {
        CustomAsrSettings {
            base_url,
            api_key: String::new(),
            model_name: "test-asr".into(),
        }
    }

    fn stream(session_id: &str, chunk: Vec<u8>) -> ProviderStream {
        let (sender, audio) = mpsc::channel(2);
        if !chunk.is_empty() {
            let _ = sender.try_send(chunk);
        }
        drop(sender);
        ProviderStream {
            session_id: session_id.to_string(),
            audio,
        }
    }

    fn pcm() -> Vec<u8> {
        1000i16.to_le_bytes().repeat(1600)
    }

    async fn run(settings: CustomAsrSettings, stream: ProviderStream) -> Vec<ProviderEvent> {
        let (events, receiver) = crossbeam_channel::unbounded();
        run_custom(stream, settings, events).await;
        receiver.into_iter().collect()
    }

    #[tokio::test]
    async fn successful_transcription_commits_trimmed_text_once() {
        let (base, request) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 19\r\n\r\n{\"text\":\" \xe4\xbd\xa0\xe5\xa5\xbd \"}",
        )
        .await;
        let events = run(settings(base), stream("c1", pcm())).await;
        assert_eq!(
            events,
            vec![
                ProviderEvent::Final {
                    session_id: "c1".into(),
                    text: "你好".into()
                },
                ProviderEvent::Complete {
                    session_id: "c1".into()
                },
                ProviderEvent::Finished {
                    session_id: "c1".into()
                },
            ]
        );
        let request = String::from_utf8_lossy(&request.await.unwrap()).to_string();
        assert!(request.starts_with("POST /v1/audio/transcriptions HTTP/1.1"));
        assert!(request.contains("name=\"file\"; filename=\"speech.wav\""));
        assert!(request.contains("name=\"model\""));
        assert!(request.contains("test-asr"));
        assert!(request.contains("name=\"response_format\""));
    }

    #[tokio::test]
    async fn empty_audio_completes_without_a_request() {
        let events = run(
            settings("http://127.0.0.1:1/v1".into()),
            stream("c2", Vec::new()),
        )
        .await;
        assert_eq!(
            events,
            vec![
                ProviderEvent::Complete {
                    session_id: "c2".into()
                },
                ProviderEvent::Finished {
                    session_id: "c2".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn unauthorized_reports_the_settings_copy() {
        let (base, _request) = serve_once(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n").await;
        let events = run(settings(base), stream("c3", pcm())).await;
        assert!(matches!(
            &events[0],
            ProviderEvent::Error { message, .. }
                if message == "自定义服务鉴权失败，请检查 API Key 和模型权限"
        ));
        assert_eq!(
            events[1],
            ProviderEvent::Finished {
                session_id: "c3".into()
            }
        );
    }

    #[tokio::test]
    async fn invalid_json_is_rejected() {
        let (base, _request) =
            serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nnope").await;
        let events = run(settings(base), stream("c4", pcm())).await;
        assert!(matches!(
            &events[0],
            ProviderEvent::Error { message, .. }
                if message == "自定义服务未返回有效的转写结果"
        ));
    }

    #[tokio::test]
    async fn missing_text_field_is_rejected() {
        let (base, _request) =
            serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n{\"x\": 1 }").await;
        let events = run(settings(base), stream("c5", pcm())).await;
        assert!(matches!(
            &events[0],
            ProviderEvent::Error { message, .. }
                if message == "自定义服务响应缺少 text 字段"
        ));
    }

    #[tokio::test]
    async fn unavailable_settings_fail_before_reading_audio() {
        let unavailable = CustomAsrSettings {
            base_url: "https://api.example.com/v1".into(),
            api_key: String::new(),
            model_name: "test-asr".into(),
        };
        let events = run(unavailable, stream("c6", pcm())).await;
        assert!(matches!(
            &events[0],
            ProviderEvent::Error { message, .. }
                if message == "请检查自定义服务的地址、API Key 和模型"
        ));
    }

    #[test]
    fn multipart_uses_the_shared_wav_header() {
        let (content_type, body) = multipart_body(&pcm(), "test-asr");
        assert!(content_type.starts_with("multipart/form-data; boundary="));
        let start = body.windows(4).position(|w| w == b"RIFF").unwrap();
        assert_eq!(&body[start + 8..start + 12], b"WAVE");
        assert_eq!(
            u32::from_le_bytes(body[start + 24..start + 28].try_into().unwrap()),
            16_000
        );
    }
}
