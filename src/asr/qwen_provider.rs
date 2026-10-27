//! Bridge from the Qwen duplex stream to engine-owned `ProviderEvent`s.

use crossbeam_channel::Sender;

use super::qwen::{QwenConfig, QwenRequestOptions};
use super::qwen_stream::{
    process_audio_stream_at, QwenStreamOutcome, QwenStreamTiming,
};
use crate::engine::{ProviderEvent, ProviderStream};

const SAMPLE_RATE: u32 = 16_000;

/// Run one utterance. Always emits `Finished`, exactly once, after any outcome.
pub async fn run_qwen(
    stream: ProviderStream,
    config: QwenConfig,
    options: QwenRequestOptions,
    events: Sender<ProviderEvent>,
    clock: fn() -> f64,
) {
    let url = match config.build_url() {
        Ok(url) => url,
        Err(error) => {
            fail_and_finish(
                &events,
                stream.session_id,
                format!("千问流式识别失败: {error}"),
            );
            return;
        }
    };
    process(
        &url,
        stream,
        &config,
        &options,
        QwenStreamTiming::default(),
        &events,
        clock,
    )
    .await;
}

async fn process(
    url: &str,
    stream: ProviderStream,
    config: &QwenConfig,
    options: &QwenRequestOptions,
    timing: QwenStreamTiming,
    events: &Sender<ProviderEvent>,
    clock: fn() -> f64,
) {
    let session_id = stream.session_id.clone();
    let mut audio = stream.audio;
    let preview_events = events.clone();
    let preview_session = session_id.clone();
    let mut on_preview = move |text: &str| {
        let _ = preview_events.send(ProviderEvent::Preview {
            session_id: preview_session.clone(),
            text: text.to_string(),
            at: clock(),
        });
    };

    let result = process_audio_stream_at(
        url,
        config,
        options,
        &mut audio,
        SAMPLE_RATE,
        timing,
        &mut on_preview,
    )
    .await;

    match result {
        Ok(QwenStreamOutcome::Completed { text }) if !text.trim().is_empty() => {
            send(
                events,
                ProviderEvent::Final {
                    session_id: session_id.clone(),
                    text,
                },
            );
            send(
                events,
                ProviderEvent::Complete {
                    session_id: session_id.clone(),
                },
            );
        }
        Ok(_) => send(
            events,
            ProviderEvent::Complete {
                session_id: session_id.clone(),
            },
        ),
        Err(error) => send(
            events,
            ProviderEvent::Error {
                session_id: session_id.clone(),
                message: error.message(),
            },
        ),
    }
    send(events, ProviderEvent::Finished { session_id });
}

fn send(events: &Sender<ProviderEvent>, event: ProviderEvent) {
    let _ = events.send(event);
}

fn fail_and_finish(events: &Sender<ProviderEvent>, session_id: String, message: String) {
    send(
        events,
        ProviderEvent::Error {
            session_id: session_id.clone(),
            message,
        },
    );
    send(events, ProviderEvent::Finished { session_id });
}

/// Convenience for callers that only have the environment-configured stream.
pub async fn run_qwen_from_env(
    stream: ProviderStream,
    events: Sender<ProviderEvent>,
    clock: fn() -> f64,
) {
    let session_id = stream.session_id.clone();
    let config = match QwenConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            fail_and_finish(
                &events,
                session_id,
                format!("千问流式识别失败: {error}"),
            );
            return;
        }
    };
    let options = config.request_options();
    run_qwen(stream, config, options, events, clock).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::qwen::QwenConfig;
    use serde_json::{json, Value};
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    fn config() -> QwenConfig {
        QwenConfig {
            api_key: "test-key".into(),
            workspace_id: String::new(),
            api_host: String::new(),
            region: "beijing".into(),
            language: "zh".into(),
            model: "qwen-audio-3.0-asr-flash-streaming".into(),
            context_enabled: false,
            recent_memory_enabled: false,
            recent_memory_count: 20,
            context_file: std::path::PathBuf::from("/nonexistent/context.txt"),
        }
    }

    fn clock() -> f64 {
        42.0
    }

    async fn listener_url() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        (listener, format!("ws://{address}/api-ws/v1/inference"))
    }

    fn stream(session_id: &str) -> (ProviderStream, mpsc::Sender<Vec<u8>>) {
        let (sender, audio) = mpsc::channel(4);
        (
            ProviderStream {
                session_id: session_id.to_string(),
                audio,
            },
            sender,
        )
    }

    async fn started(socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let _ = socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text(
                json!({"header": {"event": "task-started"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    }

    async fn read_finish(socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        while let Some(Ok(message)) = socket.next().await {
            if let Message::Text(text) = message {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["header"]["action"] == "finish-task" {
                    return;
                }
            }
        }
    }

    #[tokio::test]
    async fn completed_stream_emits_preview_final_complete_then_finished() {
        let (listener, url) = listener_url().await;
        tokio::spawn(async move {
            use futures_util::{SinkExt, StreamExt};
            use tokio_tungstenite::tungstenite::Message;
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            started(&mut socket).await;
            let _ = socket.next().await.unwrap().unwrap();
            read_finish(&mut socket).await;
            let sentence = |text: &str, end: bool| {
                json!({
                    "header": {"event": "result-generated"},
                    "payload": {"output": {"sentence": {
                        "text": text, "sentence_end": end, "sentence_id": 1,
                        "begin_time": 0, "end_time": 500,
                    }}},
                })
                .to_string()
                .into()
            };
            socket.send(Message::Text(sentence("你", false))).await.unwrap();
            socket
                .send(Message::Text(sentence("你好。", true)))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({"header": {"event": "task-finished"}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });

        let (provider, sender) = stream("s1");
        sender.send(vec![1, 2]).await.unwrap();
        drop(sender);
        let (events, receiver) = crossbeam_channel::unbounded();
        process(
            &url,
            provider,
            &config(),
            &QwenRequestOptions::default(),
            QwenStreamTiming::default(),
            &events,
            clock,
        )
        .await;
        drop(events);

        let events: Vec<ProviderEvent> = receiver.into_iter().collect();
        assert_eq!(
            events,
            vec![
                ProviderEvent::Preview {
                    session_id: "s1".into(),
                    text: "你".into(),
                    at: 42.0
                },
                ProviderEvent::Preview {
                    session_id: "s1".into(),
                    text: "你好。".into(),
                    at: 42.0
                },
                ProviderEvent::Final {
                    session_id: "s1".into(),
                    text: "你好。".into()
                },
                ProviderEvent::Complete {
                    session_id: "s1".into()
                },
                ProviderEvent::Finished {
                    session_id: "s1".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn empty_audio_completes_without_a_final() {
        let (listener, url) = listener_url().await;
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            started(&mut socket).await;
            loop {
                match tokio::time::timeout(
                    Duration::from_millis(500),
                    futures_util::StreamExt::next(&mut socket),
                )
                .await
                {
                    Ok(Some(_)) => continue,
                    _ => break,
                }
            }
        });

        let (provider, sender) = stream("s2");
        drop(sender);
        let (events, receiver) = crossbeam_channel::unbounded();
        process(
            &url,
            provider,
            &config(),
            &QwenRequestOptions::default(),
            QwenStreamTiming::default(),
            &events,
            clock,
        )
        .await;
        drop(events);

        let events: Vec<ProviderEvent> = receiver.into_iter().collect();
        assert_eq!(
            events,
            vec![
                ProviderEvent::Complete {
                    session_id: "s2".into()
                },
                ProviderEvent::Finished {
                    session_id: "s2".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn provider_failure_emits_error_then_finished() {
        let (listener, url) = listener_url().await;
        tokio::spawn(async move {
            use futures_util::{SinkExt, StreamExt};
            use tokio_tungstenite::tungstenite::Message;
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let _ = socket.next().await.unwrap().unwrap();
            socket
                .send(Message::Text(
                    json!({"header": {"event": "task-failed", "error_message": "bad model"}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });

        let (provider, sender) = stream("s3");
        drop(sender);
        let (events, receiver) = crossbeam_channel::unbounded();
        process(
            &url,
            provider,
            &config(),
            &QwenRequestOptions::default(),
            QwenStreamTiming::default(),
            &events,
            clock,
        )
        .await;
        drop(events);

        let events: Vec<ProviderEvent> = receiver.into_iter().collect();
        assert_eq!(
            events,
            vec![
                ProviderEvent::Error {
                    session_id: "s3".into(),
                    message: "千问流式识别失败: 千问 ASR 错误: bad model".into()
                },
                ProviderEvent::Finished {
                    session_id: "s3".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn invalid_host_never_connects_and_still_finishes() {
        let config = QwenConfig {
            api_host: "https://evil.example.com".into(),
            ..config()
        };
        let (provider, sender) = stream("s4");
        drop(sender);
        let (events, receiver) = crossbeam_channel::unbounded();
        run_qwen(
            provider,
            config,
            QwenRequestOptions::default(),
            events,
            clock,
        )
        .await;

        let events: Vec<ProviderEvent> = receiver.into_iter().collect();
        assert_eq!(
            events,
            vec![
                ProviderEvent::Error {
                    session_id: "s4".into(),
                    message:
                        "千问流式识别失败: 千问 API Host 必须是 aliyuncs.com 官方域名".into()
                },
                ProviderEvent::Finished {
                    session_id: "s4".into()
                },
            ]
        );
    }
}
