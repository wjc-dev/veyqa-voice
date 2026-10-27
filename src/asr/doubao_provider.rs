//! Bridge from the Doubao SAUC binary stream to engine-owned `ProviderEvent`s.
//!
//! Mirrors `DoubaoStreamingProcessor.process_audio_stream`: connect with the
//! three auth headers, send the full request, stream gzip audio frames, then
//! finish with a negative-sequence empty frame and wait up to 8 s for the
//! final result.

use std::time::Duration;

use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use super::doubao::{
    parse_response, DoubaoProtocol, RequestSettings, DEFAULT_SAMPLE_RATE,
};
use super::speech_models::selected_model;
use crate::config;
use crate::engine::{ProviderEvent, ProviderStream};

pub const WS_URL: &str = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async";
const HEARTBEAT_SECONDS: u64 = 20;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone)]
pub struct DoubaoConfig {
    pub app_key: String,
    pub access_key: String,
    pub model: String,
}

impl DoubaoConfig {
    pub fn from_env() -> Result<Self, String> {
        let app_key = config::var("DOUBAO_APP_KEY");
        let access_key = config::var("DOUBAO_ACCESS_KEY");
        if app_key.is_empty() || access_key.is_empty() {
            return Err("豆包 App ID 或 Access Token 未配置".to_string());
        }
        let model = selected_model("doubao", &config::var("DOUBAO_ASR_RESOURCE_ID"))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            app_key,
            access_key,
            model,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DoubaoTiming {
    pub connect: Duration,
    pub initial: Duration,
    pub send: Duration,
    pub final_response: Duration,
}

impl Default for DoubaoTiming {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            initial: Duration::from_secs(10),
            send: Duration::from_secs(5),
            final_response: Duration::from_secs(8),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoubaoError {
    Authentication { status: u16 },
    Provider(String),
}

impl DoubaoError {
    pub fn message(&self) -> String {
        match self {
            Self::Authentication { status } => format!(
                "豆包鉴权失败（HTTP {status}），请检查 App Key、Access Key 和资源 ID"
            ),
            Self::Provider(message) => message.clone(),
        }
    }

    fn provider(message: impl Into<String>) -> Self {
        Self::Provider(message.into())
    }
}

/// Run one utterance. Always emits `Finished`, exactly once, after any outcome.
pub async fn run_doubao(
    stream: ProviderStream,
    config: DoubaoConfig,
    settings: RequestSettings,
    events: Sender<ProviderEvent>,
    clock: fn() -> f64,
) {
    run_at(
        WS_URL,
        stream,
        &config,
        &settings,
        DoubaoTiming::default(),
        &events,
        clock,
    )
    .await;
}

async fn run_at(
    url: &str,
    stream: ProviderStream,
    config: &DoubaoConfig,
    settings: &RequestSettings,
    timing: DoubaoTiming,
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
    let result = stream_once(url, config, settings, &mut audio, timing, &mut on_preview).await;
    match result {
        Ok(Some(text)) => {
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
        Ok(None) => send(
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

async fn stream_once(
    url: &str,
    config: &DoubaoConfig,
    settings: &RequestSettings,
    audio: &mut mpsc::Receiver<Vec<u8>>,
    timing: DoubaoTiming,
    on_preview: &mut (dyn FnMut(&str) + Send),
) -> Result<Option<String>, DoubaoError> {
    let mut socket = connect(url, config, timing).await?;
    let mut protocol = DoubaoProtocol::new();
    send_binary(
        &mut socket,
        protocol.full_request(DEFAULT_SAMPLE_RATE, settings),
        timing,
    )
    .await?;

    // The server may answer the full request immediately; a timeout is not
    // fatal because the first audio frame can trigger the response instead.
    match tokio::time::timeout(timing.initial, socket.next()).await {
        Ok(None) => return Err(DoubaoError::provider("豆包连接在会话初始化时关闭")),
        Ok(Some(Err(error))) => return Err(DoubaoError::provider(error.to_string())),
        Ok(Some(Ok(Message::Close(_)))) => {
            return Err(DoubaoError::provider("豆包连接在会话初始化时关闭"))
        }
        Ok(Some(Ok(Message::Binary(frame)))) => {
            let initial = parse_response(&frame);
            if let Some(error) = initial.error {
                close(socket).await;
                return Err(DoubaoError::provider(error));
            }
        }
        Ok(Some(Ok(_))) => {}
        Err(_) => {}
    }

    let mut latest = String::new();
    let mut finish_sent = false;
    let mut final_deadline: Option<Instant> = None;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECONDS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await;

    let result = loop {
        let deadline_branch = async {
            match final_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = deadline_branch => {
                break Err(DoubaoError::provider("等待豆包最终识别结果超时"));
            }
            _ = heartbeat.tick(), if !finish_sent => {
                let _ = socket.send(Message::Ping(Vec::new().into())).await;
            }
            chunk = audio.recv(), if !finish_sent => {
                match chunk {
                    Some(chunk) => {
                        send_binary(&mut socket, protocol.audio_request(&chunk, false), timing).await?;
                    }
                    None => {
                        send_binary(&mut socket, protocol.audio_request(&[], true), timing).await?;
                        finish_sent = true;
                        final_deadline = Some(Instant::now() + timing.final_response);
                    }
                }
            }
            message = socket.next() => {
                match message {
                    Some(Ok(Message::Binary(frame))) => {
                        let parsed = parse_response(&frame);
                        if let Some(error) = parsed.error {
                            break Err(DoubaoError::provider(error));
                        }
                        let candidate = format!("{}{}", parsed.definite_text, parsed.pending_text);
                        if !candidate.is_empty() {
                            latest = candidate.clone();
                            on_preview(&candidate);
                        }
                        if parsed.is_final {
                            break Ok((!latest.is_empty()).then_some(latest));
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        break Err(DoubaoError::provider("豆包连接提前关闭"));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        break Err(DoubaoError::provider(format!("豆包 WebSocket 错误: {error}")));
                    }
                }
            }
        }
    };
    close(socket).await;
    result
}

async fn connect(
    url: &str,
    config: &DoubaoConfig,
    timing: DoubaoTiming,
) -> Result<Socket, DoubaoError> {
    let mut request = url
        .into_client_request()
        .map_err(|error| DoubaoError::provider(error.to_string()))?;
    {
        let headers = request.headers_mut();
        let mut insert = |name: &'static str, value: &str| {
            HeaderValue::from_str(value)
                .map(|value| {
                    headers.insert(name, value);
                })
                .map_err(|error| DoubaoError::provider(error.to_string()))
        };
        insert("X-Api-Resource-Id", &config.model)?;
        insert("X-Api-Connect-Id", &uuid::Uuid::new_v4().to_string())?;
        insert("X-Api-Access-Key", &config.access_key)?;
        insert("X-Api-App-Key", &config.app_key)?;
    }

    let connected = if url.starts_with("wss://") {
        let connector = Connector::NativeTls(
            native_tls::TlsConnector::builder()
                .build()
                .map_err(|error| DoubaoError::provider(error.to_string()))?,
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
        Err(_) => Err(DoubaoError::provider("连接豆包超时")),
        Ok(Err(WsError::Http(response))) => {
            let status = response.status().as_u16();
            if matches!(status, 401 | 403) {
                Err(DoubaoError::Authentication { status })
            } else {
                Err(DoubaoError::provider(format!(
                    "豆包连接失败（HTTP {status}）"
                )))
            }
        }
        Ok(Err(error)) => Err(DoubaoError::provider(error.to_string())),
        Ok(Ok((socket, response))) => {
            let status = response.status().as_u16();
            if status == 101 {
                Ok(socket)
            } else if matches!(status, 401 | 403) {
                Err(DoubaoError::Authentication { status })
            } else {
                Err(DoubaoError::provider(format!(
                    "豆包连接失败（HTTP {status}）"
                )))
            }
        }
    }
}

async fn send_binary(
    socket: &mut Socket,
    payload: Vec<u8>,
    timing: DoubaoTiming,
) -> Result<(), DoubaoError> {
    match tokio::time::timeout(timing.send, socket.send(Message::Binary(payload.into()))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(DoubaoError::provider(error.to_string())),
        Err(_) => Err(DoubaoError::provider("发送超时")),
    }
}

async fn close(mut socket: Socket) {
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, socket.close(None)).await;
}

fn send(events: &Sender<ProviderEvent>, event: ProviderEvent) {
    let _ = events.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use serde_json::{json, Value};
    use std::io::Write;
    use tokio::net::TcpListener;

    fn config() -> DoubaoConfig {
        DoubaoConfig {
            app_key: "app".into(),
            access_key: "key".into(),
            model: "volc.seedasr.sauc.duration".into(),
        }
    }

    fn clock() -> f64 {
        7.0
    }

    async fn listener_url() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        (listener, format!("ws://{address}/api/v3/sauc/bigmodel_async"))
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

    fn gzip(body: &Value) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(body.to_string().as_bytes())
            .unwrap();
        encoder.finish().unwrap()
    }

    fn server_frame(body: &Value, final_frame: bool) -> Vec<u8> {
        let compressed = gzip(body);
        let mut packet = vec![0x11];
        packet.push((0b1001 << 4) | if final_frame { 0b0011 } else { 0b0001 });
        packet.push((0b0001 << 4) | 0b0001);
        packet.push(0);
        packet.extend_from_slice(&1i32.to_be_bytes());
        packet.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
        packet.extend_from_slice(&compressed);
        packet
    }

    fn error_frame(code: i32, message: &str) -> Vec<u8> {
        let mut packet = vec![0x11, (0b1111 << 4) | 0b0011, 0x10, 0];
        packet.extend_from_slice(&1i32.to_be_bytes());
        packet.extend_from_slice(&code.to_be_bytes());
        packet.extend_from_slice(&0i32.to_be_bytes());
        packet.extend_from_slice(message.as_bytes());
        packet
    }

    fn utterance(text: &str, definite: bool) -> Value {
        json!({
            "result": {
                "utterances": [{"text": text, "definite": definite}],
            }
        })
    }

    async fn next_binary(
        socket: &mut WebSocketStream<TcpStream>,
    ) -> Vec<u8> {
        loop {
            match socket.next().await {
                Some(Ok(Message::Binary(data))) => return data.to_vec(),
                Some(Ok(_)) => continue,
                other => panic!("expected binary frame, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn completed_stream_emits_preview_final_complete_then_finished() {
        let (listener, url) = listener_url().await;
        tokio::spawn(async move {
            use tokio_tungstenite::tungstenite::Message;
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let _full = next_binary(&mut socket).await;
            // The first response slot only checks for errors, matching Python,
            // so the server acknowledges the full request with an empty result.
            socket
                .send(Message::Binary(
                    server_frame(&json!({"result": {}}), false).into(),
                ))
                .await
                .unwrap();
            let _audio = next_binary(&mut socket).await;
            socket
                .send(Message::Binary(
                    server_frame(&utterance("你", false), false).into(),
                ))
                .await
                .unwrap();
            let _final_audio = next_binary(&mut socket).await;
            socket
                .send(Message::Binary(
                    server_frame(&utterance("你好。", true), true).into(),
                ))
                .await
                .unwrap();
        });

        let (provider, sender) = stream("s1");
        sender.send(vec![1, 2]).await.unwrap();
        drop(sender);
        let (events, receiver) = crossbeam_channel::unbounded();
        run_at(
            &url,
            provider,
            &config(),
            &RequestSettings::default(),
            DoubaoTiming::default(),
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
                    at: 7.0
                },
                ProviderEvent::Preview {
                    session_id: "s1".into(),
                    text: "你好。".into(),
                    at: 7.0
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
    async fn empty_audio_still_finishes_without_a_final() {
        let (listener, url) = listener_url().await;
        tokio::spawn(async move {
            use tokio_tungstenite::tungstenite::Message;
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let _full = next_binary(&mut socket).await;
            let _final_audio = next_binary(&mut socket).await;
            socket
                .send(Message::Binary(
                    server_frame(&json!({"result": {}}), true).into(),
                ))
                .await
                .unwrap();
        });

        let (provider, sender) = stream("s2");
        drop(sender);
        let (events, receiver) = crossbeam_channel::unbounded();
        run_at(
            &url,
            provider,
            &config(),
            &RequestSettings::default(),
            DoubaoTiming::default(),
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
    async fn server_error_emits_error_then_finished() {
        let (listener, url) = listener_url().await;
        tokio::spawn(async move {
            use tokio_tungstenite::tungstenite::Message;
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let _full = next_binary(&mut socket).await;
            socket
                .send(Message::Binary(error_frame(45000001, "bad request").into()))
                .await
                .unwrap();
        });

        let (provider, sender) = stream("s3");
        drop(sender);
        let (events, receiver) = crossbeam_channel::unbounded();
        run_at(
            &url,
            provider,
            &config(),
            &RequestSettings::default(),
            DoubaoTiming::default(),
            &events,
            clock,
        )
        .await;
        drop(events);

        let events: Vec<ProviderEvent> = receiver.into_iter().collect();
        assert_eq!(events.len(), 2);
        match &events[0] {
            ProviderEvent::Error { session_id, message } => {
                assert_eq!(session_id, "s3");
                assert!(message.contains("45000001"));
                assert!(message.contains("bad request"));
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert_eq!(
            events[1],
            ProviderEvent::Finished {
                session_id: "s3".into()
            }
        );
    }
}
