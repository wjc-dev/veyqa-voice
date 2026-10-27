//! One push-to-talk utterance over DashScope's duplex WebSocket protocol.
//!
//! Mirrors `QwenStreamingProcessor.process_audio_stream`: connect, run-task,
//! wait for task-started, stream PCM, finish-task, then wait for the final
//! transcript. Empty audio finishes locally and is never sent.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};
use tokio::net::TcpStream;

use super::qwen::{
    finish_task_event, QwenConfig, QwenEvent, QwenRequestOptions, TranscriptAssembler,
};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

const HEARTBEAT_SECONDS: u64 = 20;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QwenStreamTiming {
    pub connect: Duration,
    pub send: Duration,
    pub final_response: Duration,
}

impl Default for QwenStreamTiming {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            send: Duration::from_secs(5),
            final_response: Duration::from_secs(20),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QwenStreamError {
    Authentication { status: u16 },
    Provider(String),
}

impl QwenStreamError {
    pub fn message(&self) -> String {
        match self {
            Self::Authentication { status } => format!(
                "千问鉴权失败（HTTP {status}），请检查 API Key、地域和 Workspace ID"
            ),
            Self::Provider(message) => message.clone(),
        }
    }

    pub fn is_authentication(&self) -> bool {
        matches!(self, Self::Authentication { .. })
    }

    fn provider(message: impl Into<String>) -> Self {
        Self::Provider(message.into())
    }

    fn wrapped(error: impl std::fmt::Display) -> Self {
        Self::Provider(format!("千问流式识别失败: {error}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QwenStreamOutcome {
    Completed { text: String },
    /// A very short tap produced no audio; nothing was sent.
    EmptyAudio,
}

/// Stream one utterance. Preview callbacks receive the cumulative local text.
pub async fn process_audio_stream(
    cfg: &QwenConfig,
    options: &QwenRequestOptions,
    audio: &mut mpsc::Receiver<Vec<u8>>,
    sample_rate: u32,
    on_preview: &mut (dyn FnMut(&str) + Send),
) -> Result<QwenStreamOutcome, QwenStreamError> {
    let url = cfg.build_url().map_err(QwenStreamError::wrapped)?;
    process_audio_stream_at(
        &url,
        cfg,
        options,
        audio,
        sample_rate,
        QwenStreamTiming::default(),
        on_preview,
    )
    .await
}

pub(crate) async fn process_audio_stream_at(
    url: &str,
    cfg: &QwenConfig,
    options: &QwenRequestOptions,
    audio: &mut mpsc::Receiver<Vec<u8>>,
    sample_rate: u32,
    timing: QwenStreamTiming,
    on_preview: &mut (dyn FnMut(&str) + Send),
) -> Result<QwenStreamOutcome, QwenStreamError> {
    let mut socket = connect(url, cfg, timing).await?;
    let result = run(&mut socket, cfg, options, audio, sample_rate, timing, on_preview).await;
    close(socket).await;
    result
}

async fn run(
    socket: &mut Socket,
    cfg: &QwenConfig,
    options: &QwenRequestOptions,
    audio: &mut mpsc::Receiver<Vec<u8>>,
    sample_rate: u32,
    timing: QwenStreamTiming,
    on_preview: &mut (dyn FnMut(&str) + Send),
) -> Result<QwenStreamOutcome, QwenStreamError> {
    let task_id = uuid::Uuid::new_v4().simple().to_string();
    let task_id = &task_id[..32];
    send_json(socket, cfg.run_task_event(task_id, sample_rate, options), timing).await?;
    wait_until_ready(socket, timing).await?;

    let mut assembler = TranscriptAssembler::default();
    let mut chunk_count = 0u64;
    let mut finish_sent = false;
    let mut final_deadline: Option<Instant> = None;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECONDS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await;

    loop {
        let deadline = final_deadline;
        // Never leave the receiver blocking past the final-response deadline.
        let deadline_branch = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            _ = deadline_branch => {
                return Err(QwenStreamError::provider("等待千问最终识别结果超时"));
            }
            _ = heartbeat.tick(), if !finish_sent => {
                // Best-effort keepalive; a failure surfaces on the next read.
                let _ = socket.send(Message::Ping(Vec::new().into())).await;
            }
            chunk = audio.recv(), if !finish_sent => {
                match chunk {
                    Some(chunk) => {
                        chunk_count += 1;
                        send_audio(socket, chunk, timing).await?;
                    }
                    None => {
                        if chunk_count == 0 {
                            // A very short tap can finish before the first
                            // callback; committing an empty buffer only earns
                            // a provider error, so end locally and silently.
                            return Ok(QwenStreamOutcome::EmptyAudio);
                        }
                        send_json(socket, finish_task_event(task_id), timing).await?;
                        finish_sent = true;
                        final_deadline = Some(Instant::now() + timing.final_response);
                    }
                }
            }
            message = socket.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let value: Value = serde_json::from_str(text.as_ref())
                            .map_err(QwenStreamError::wrapped)?;
                        match assembler.handle(&value) {
                            QwenEvent::Error(error) => {
                                return Err(QwenStreamError::provider(error));
                            }
                            QwenEvent::Update { preview } => on_preview(&preview),
                            QwenEvent::TaskFinished => {
                                return Ok(QwenStreamOutcome::Completed {
                                    text: assembler.final_text(),
                                });
                            }
                            QwenEvent::Ignored => {}
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        return Err(QwenStreamError::provider("千问连接提前关闭"));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        return Err(QwenStreamError::provider(format!(
                            "千问 WebSocket 错误: {error}"
                        )));
                    }
                }
            }
        }
    }
}

async fn connect(
    url: &str,
    cfg: &QwenConfig,
    timing: QwenStreamTiming,
) -> Result<Socket, QwenStreamError> {
    let mut request = url
        .into_client_request()
        .map_err(QwenStreamError::wrapped)?;
    {
        let headers = request.headers_mut();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", cfg.api_key))
                .map_err(QwenStreamError::wrapped)?,
        );
        headers.insert(
            "User-Agent",
            HeaderValue::from_static("Whisper-Input-Next/Qwen-ASR"),
        );
        // A workspace-specific host already identifies the workspace; sending
        // a separately entered workspace header can break a valid request.
        if !cfg.workspace_id.is_empty() && cfg.api_host.is_empty() {
            headers.insert(
                "X-DashScope-WorkSpace",
                HeaderValue::from_str(&cfg.workspace_id).map_err(QwenStreamError::wrapped)?,
            );
        }
    }

    let connected = if url.starts_with("wss://") {
        let connector = Connector::NativeTls(
            native_tls::TlsConnector::builder()
                .build()
                .map_err(QwenStreamError::wrapped)?,
        );
        tokio::time::timeout(
            timing.connect,
            tokio_tungstenite::connect_async_tls_with_config(request, None, true, Some(connector)),
        )
        .await
    } else {
        tokio::time::timeout(
            timing.connect,
            tokio_tungstenite::connect_async_with_config(request, None, true),
        )
        .await
    };

    match connected {
        Err(_) => Err(QwenStreamError::wrapped("连接 DashScope 超时")),
        Ok(Err(WsError::Http(response))) => Err(QwenStreamError::Authentication {
            status: response.status().as_u16(),
        }),
        Ok(Err(error)) => Err(QwenStreamError::wrapped(error)),
        Ok(Ok((socket, response))) => {
            let status = response.status().as_u16();
            if status != 101 {
                return Err(QwenStreamError::Authentication { status });
            }
            Ok(socket)
        }
    }
}

async fn wait_until_ready(
    socket: &mut Socket,
    timing: QwenStreamTiming,
) -> Result<(), QwenStreamError> {
    let deadline = Instant::now() + timing.connect;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(QwenStreamError::wrapped("等待千问 task-started 超时"));
        }
        match tokio::time::timeout(remaining, socket.next()).await {
            Err(_) => return Err(QwenStreamError::wrapped("等待千问 task-started 超时")),
            Ok(None) => {
                return Err(QwenStreamError::wrapped("千问连接在会话初始化时关闭"));
            }
            Ok(Some(Err(error))) => return Err(QwenStreamError::wrapped(error)),
            Ok(Some(Ok(Message::Text(text)))) => {
                let value: Value =
                    serde_json::from_str(text.as_ref()).map_err(QwenStreamError::wrapped)?;
                let event = value
                    .get("header")
                    .and_then(|header| header.get("event"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if event == "task-failed" {
                    let mut assembler = TranscriptAssembler::default();
                    if let QwenEvent::Error(error) = assembler.handle(&value) {
                        return Err(QwenStreamError::wrapped(error));
                    }
                    return Err(QwenStreamError::wrapped("未知服务端错误"));
                }
                if event == "task-started" {
                    return Ok(());
                }
            }
            Ok(Some(Ok(Message::Close(_)))) => {
                return Err(QwenStreamError::wrapped("千问连接在会话初始化时关闭"));
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

async fn send_json(
    socket: &mut Socket,
    value: Value,
    timing: QwenStreamTiming,
) -> Result<(), QwenStreamError> {
    send(socket, Message::Text(value.to_string().into()), timing).await
}

async fn send_audio(
    socket: &mut Socket,
    chunk: Vec<u8>,
    timing: QwenStreamTiming,
) -> Result<(), QwenStreamError> {
    send(socket, Message::Binary(chunk.into()), timing).await
}

async fn send(
    socket: &mut Socket,
    message: Message,
    timing: QwenStreamTiming,
) -> Result<(), QwenStreamError> {
    match tokio::time::timeout(timing.send, socket.send(message)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(QwenStreamError::wrapped(error)),
        Err(_) => Err(QwenStreamError::wrapped("发送超时")),
    }
}

async fn close(mut socket: Socket) {
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, socket.close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio::time::timeout;

    fn test_config() -> QwenConfig {
        QwenConfig {
            api_key: "test-key".to_string(),
            workspace_id: String::new(),
            api_host: String::new(),
            region: "beijing".to_string(),
            language: "zh".to_string(),
            model: "qwen-audio-3.0-asr-flash-streaming".to_string(),
            context_enabled: false,
            recent_memory_enabled: false,
            recent_memory_count: 20,
            context_file: std::path::PathBuf::from("/nonexistent/personal_context.txt"),
        }
    }

    fn fast_timing() -> QwenStreamTiming {
        QwenStreamTiming {
            connect: Duration::from_secs(1),
            send: Duration::from_secs(1),
            final_response: Duration::from_millis(800),
        }
    }

    async fn listener_url() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        (listener, format!("ws://{address}/api-ws/v1/inference"))
    }

    #[tokio::test]
    async fn streams_audio_and_commits_the_final_text() {
        let (listener, url) = listener_url().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let run = socket.next().await.unwrap().unwrap();
            let run: Value = serde_json::from_str(run.to_text().unwrap()).unwrap();
            assert_eq!(run["header"]["action"], "run-task");
            assert_eq!(run["payload"]["model"], "qwen-audio-3.0-asr-flash-streaming");
            assert_eq!(run["payload"]["parameters"]["sample_rate"], 16000);
            socket
                .send(Message::Text(
                    json!({"header": {"event": "task-started"}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();

            let mut chunks = 0;
            let mut saw_finish = false;
            while let Some(message) = socket.next().await {
                match message.unwrap() {
                    Message::Binary(data) => {
                        chunks += 1;
                        assert_eq!(data.as_ref(), &[1u8, 2][..]);
                    }
                    Message::Text(text) => {
                        let value: Value = serde_json::from_str(&text).unwrap();
                        if value["header"]["action"] == "finish-task" {
                            saw_finish = true;
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            assert!(saw_finish);
            socket
                .send(Message::Text(
                    json!({
                        "header": {"event": "result-generated"},
                        "payload": {"output": {"sentence": {
                            "text": "你好",
                            "sentence_end": true,
                            "sentence_id": 1,
                            "begin_time": 0,
                            "end_time": 500,
                        }}}
                    })
                    .to_string()
                    .into(),
                ))
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
            chunks
        });

        let cfg = test_config();
        let mut options = QwenRequestOptions::default();
        options.context = "背景".to_string();
        let (sender, mut audio) = mpsc::channel(4);
        sender.send(vec![1, 2]).await.unwrap();
        sender.send(vec![1, 2]).await.unwrap();
        drop(sender);

        let mut previews = Vec::new();
        let outcome = process_audio_stream_at(
            &url,
            &cfg,
            &options,
            &mut audio,
            16_000,
            fast_timing(),
            &mut |text| previews.push(text.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(
            outcome,
            QwenStreamOutcome::Completed {
                text: "你好".to_string()
            }
        );
        assert_eq!(previews, ["你好"]);
        assert_eq!(server.await.unwrap(), 2);
    }

    #[tokio::test]
    async fn empty_audio_is_not_sent_and_completes_locally() {
        let (listener, url) = listener_url().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = socket.next().await.unwrap().unwrap();
            socket
                .send(Message::Text(
                    json!({"header": {"event": "task-started"}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let mut saw_finish = false;
            while let Ok(Some(Ok(message))) = timeout(Duration::from_millis(500), socket.next()).await
            {
                if let Message::Text(text) = message {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["header"]["action"] == "finish-task" {
                        saw_finish = true;
                    }
                }
            }
            saw_finish
        });

        let (sender, mut audio) = mpsc::channel::<Vec<u8>>(1);
        drop(sender);
        let outcome = process_audio_stream_at(
            &url,
            &test_config(),
            &QwenRequestOptions::default(),
            &mut audio,
            16_000,
            fast_timing(),
            &mut |_| {},
        )
        .await
        .unwrap();

        assert_eq!(outcome, QwenStreamOutcome::EmptyAudio);
        assert!(!server.await.unwrap());
    }

    #[tokio::test]
    async fn task_failed_during_initialization_is_reported() {
        let (listener, url) = listener_url().await;
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
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

        let (_sender, mut audio) = mpsc::channel::<Vec<u8>>(1);
        let error = process_audio_stream_at(
            &url,
            &test_config(),
            &QwenRequestOptions::default(),
            &mut audio,
            16_000,
            fast_timing(),
            &mut |_| {},
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.message(),
            "千问流式识别失败: 千问 ASR 错误: bad model"
        );
    }

    #[tokio::test]
    async fn early_close_after_finish_is_an_error() {
        let (listener, url) = listener_url().await;
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = socket.next().await.unwrap().unwrap();
            socket
                .send(Message::Text(
                    json!({"header": {"event": "task-started"}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            while let Some(Ok(message)) = socket.next().await {
                if let Message::Text(text) = message {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["header"]["action"] == "finish-task" {
                        break;
                    }
                }
            }
            let _ = socket.close(None).await;
        });

        let (sender, mut audio) = mpsc::channel(2);
        sender.send(vec![1, 2]).await.unwrap();
        drop(sender);

        let error = process_audio_stream_at(
            &url,
            &test_config(),
            &QwenRequestOptions::default(),
            &mut audio,
            16_000,
            fast_timing(),
            &mut |_| {},
        )
        .await
        .unwrap_err();

        assert_eq!(error.message(), "千问连接提前关闭");
    }

    #[tokio::test]
    async fn missing_final_response_times_out() {
        let (listener, url) = listener_url().await;
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = socket.next().await.unwrap().unwrap();
            socket
                .send(Message::Text(
                    json!({"header": {"event": "task-started"}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            // Read forever; never finish the task.
            while socket.next().await.is_some() {}
        });

        let (sender, mut audio) = mpsc::channel(2);
        sender.send(vec![1, 2]).await.unwrap();
        drop(sender);

        let timing = QwenStreamTiming {
            final_response: Duration::from_millis(150),
            ..fast_timing()
        };
        let error = process_audio_stream_at(
            &url,
            &test_config(),
            &QwenRequestOptions::default(),
            &mut audio,
            16_000,
            timing,
            &mut |_| {},
        )
        .await
        .unwrap_err();

        assert_eq!(error.message(), "等待千问最终识别结果超时");
    }

    #[tokio::test]
    async fn handshake_401_maps_to_the_authentication_message() {
        use tokio::io::AsyncWriteExt;

        let (listener, url) = listener_url().await;
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = stream
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        });

        let (_sender, mut audio) = mpsc::channel::<Vec<u8>>(1);
        let error = process_audio_stream_at(
            &url,
            &test_config(),
            &QwenRequestOptions::default(),
            &mut audio,
            16_000,
            fast_timing(),
            &mut |_| {},
        )
        .await
        .unwrap_err();

        assert!(error.is_authentication());
        assert_eq!(
            error.message(),
            "千问鉴权失败（HTTP 401），请检查 API Key、地域和 Workspace ID"
        );
    }
}
