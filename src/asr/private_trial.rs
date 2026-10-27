//! Offline-provisioned trial credential, mirroring `private_trial_payload.py`.
//! This is packaging convenience, not protection against extraction.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Result};
use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::Connector;

use crate::diagnostics::DiagnosticsStore;
use crate::paths;

const MAGIC: &[u8] = b"VEYQA-PRIVATE-1\0";
const FILENAME: &str = "private-trial.dat";
const PAYLOAD_MODEL: &str = "qwen-audio-3.0-asr-flash-streaming";
const ERROR_UNAVAILABLE: &str = "内测凭证无法读取，请联系分发者获取新的内测包。";

#[derive(Clone)]
pub struct TrialCredential {
    pub key: String,
    pub host: String,
}

impl std::fmt::Debug for TrialCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TrialCredential")
            .field("key", &"<redacted>")
            .field("host", &self.host)
            .finish()
    }
}

fn mask(data: &[u8], seed: &[u8]) -> Vec<u8> {
    let mut stream = Vec::with_capacity(data.len() + 32);
    for index in 0..(data.len() + 31) / 32 {
        let mut hasher = Sha256::new();
        hasher.update(seed);
        hasher.update((index as u32).to_be_bytes());
        stream.extend_from_slice(&hasher.finalize());
    }
    data.iter()
        .zip(stream.iter())
        .map(|(value, key)| value ^ key)
        .collect()
}

fn validate(key: &str, host: &str) -> Result<TrialCredential> {
    let key_ok = (20..=1024).contains(&key.chars().count())
        && Regex::new(r"^sk-[A-Za-z0-9._-]+$")
            .expect("static key pattern")
            .is_match(key);
    let host_ok = Regex::new(r"^[a-z0-9-]+\.cn-beijing\.maas\.aliyuncs\.com$")
        .expect("static host pattern")
        .is_match(host);
    if !key_ok || !host_ok {
        bail!("Invalid private trial credential configuration");
    }
    Ok(TrialCredential {
        key: key.to_string(),
        host: host.to_string(),
    })
}

pub fn decode(payload: &[u8]) -> Result<TrialCredential> {
    if payload.len() <= MAGIC.len() + 64 || payload.len() > 4096 || !payload.starts_with(MAGIC) {
        bail!("Private trial payload is missing or damaged");
    }
    let seed = &payload[MAGIC.len()..MAGIC.len() + 32];
    let digest = &payload[MAGIC.len() + 32..MAGIC.len() + 64];
    let data = mask(&payload[MAGIC.len() + 64..], seed);
    if Sha256::digest(&data).as_slice() != digest {
        bail!("Private trial payload is missing or damaged");
    }
    let value: Value = serde_json::from_slice(&data)
        .map_err(|_| anyhow::anyhow!("Private trial payload is missing or damaged"))?;
    if value.get("model").and_then(Value::as_str) != Some(PAYLOAD_MODEL) {
        bail!("Private trial payload is missing or damaged");
    }
    validate(
        value.get("key").and_then(Value::as_str).unwrap_or(""),
        value.get("host").and_then(Value::as_str).unwrap_or(""),
    )
    .map_err(|_| anyhow::anyhow!("Private trial payload is missing or damaged"))
}

pub fn payload_path() -> PathBuf {
    paths::resource_root().join(FILENAME)
}

pub fn load() -> Result<TrialCredential> {
    let bytes = std::fs::read(payload_path()).map_err(|_| anyhow::anyhow!(ERROR_UNAVAILABLE))?;
    decode(&bytes).map_err(|_| anyhow::anyhow!(ERROR_UNAVAILABLE))
}

pub fn available() -> bool {
    crate::release_features::private_trial_enabled() && load().is_ok()
}

fn publish(values: Map<String, Value>) {
    let _ = DiagnosticsStore::new(Some(paths::promotion_file()), false).update(&values);
}

/// Mirrors Python `PrivateTrialBroker.request_check`: a tiny silent utterance
/// proves the credential works before the settings page applies it.
pub async fn check_and_publish(request_id: String) {
    let (status, enabled) = match probe().await {
        Ok(()) => ("内置服务已连接，正在保存并应用…", true),
        Err(_) => ("内置服务连接失败，请检查网络或切换个人 API。", false),
    };
    let mut values = Map::new();
    values.insert("status".to_string(), json!(status));
    values.insert("enabled".to_string(), json!(enabled));
    values.insert(
        "model".to_string(),
        json!(crate::asr::speech_models::promotion_model().unwrap_or_default()),
    );
    values.insert("remaining_seconds".to_string(), json!(3600));
    values.insert("request_id".to_string(), json!(request_id));
    values.insert("private_trial".to_string(), json!(true));
    publish(values);
}

pub(crate) async fn probe() -> Result<()> {
    let credential = load()?;
    let config = crate::asr::qwen::QwenConfig::for_builtin(&credential)?;
    let url = config.build_url()?;
    let mut request = url.into_client_request()?;
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", config.api_key))?,
    );
    let connector = Connector::NativeTls(native_tls::TlsConnector::builder().build()?);
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_tls_with_config(request, None, true, Some(connector)),
    )
    .await??;

    let task_id = uuid::Uuid::new_v4().simple().to_string();
    socket
        .send(Message::Text(
            json!({
                "header": {"action": "run-task", "task_id": task_id, "streaming": "duplex"},
                "payload": {
                    "task_group": "audio",
                    "task": "asr",
                    "function": "recognition",
                    "model": config.model,
                    "parameters": {"format": "pcm", "sample_rate": 16000},
                    "input": {}
                }
            })
            .to_string()
            .into(),
        ))
        .await?;

    for _ in 0..8 {
        let message = tokio::time::timeout(Duration::from_secs(8), socket.next())
            .await?
            .ok_or_else(|| anyhow::anyhow!("Trial connection unavailable"))??;
        let Message::Text(text) = message else {
            break;
        };
        let event = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("header")
                    .and_then(|header| header.get("event"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        match event.as_str() {
            "task-started" => {
                // Provider ACL failures can arrive after task-started.
                socket.send(Message::Binary(vec![0; 3200].into())).await?;
                socket
                    .send(Message::Text(
                        json!({
                            "header": {"action": "finish-task", "task_id": task_id, "streaming": "duplex"},
                            "payload": {"input": {}}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await?;
            }
            "task-finished" => return Ok(()),
            "task-failed" => bail!("Trial connection unavailable"),
            _ => {}
        }
    }
    bail!("Trial connection unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_for_test(key: &str, host: &str) -> Vec<u8> {
        let data = json!({"key": key, "host": host, "model": PAYLOAD_MODEL}).to_string();
        let seed = [7u8; 32];
        let mut payload = MAGIC.to_vec();
        payload.extend_from_slice(&seed);
        payload.extend_from_slice(&Sha256::digest(data.as_bytes()));
        payload.extend_from_slice(&mask(data.as_bytes(), &seed));
        payload
    }

    #[test]
    fn decodes_a_well_formed_payload() {
        let credential =
            decode(&encode_for_test("sk-test_1234567890.abc", "ws.cn-beijing.maas.aliyuncs.com"))
                .unwrap();
        assert_eq!(credential.key, "sk-test_1234567890.abc");
        assert_eq!(credential.host, "ws.cn-beijing.maas.aliyuncs.com");
    }

    #[test]
    fn rejects_damaged_or_mismatched_payloads() {
        let mut payload =
            encode_for_test("sk-test_1234567890.abc", "ws.cn-beijing.maas.aliyuncs.com");
        let last = payload.len() - 1;
        payload[last] ^= 1;
        assert!(decode(&payload).is_err());
        assert!(decode(b"not a payload").is_err());
    }

    #[test]
    fn rejects_bad_keys_and_hosts() {
        assert!(decode(&encode_for_test("sk-", "ws.cn-beijing.maas.aliyuncs.com")).is_err());
        assert!(decode(&encode_for_test(
            "sk-test_1234567890.abc",
            "evil.example.com"
        ))
        .is_err());
    }

    #[test]
    fn credential_debug_never_prints_the_key() {
        let credential =
            decode(&encode_for_test("sk-test_1234567890.abc", "ws.cn-beijing.maas.aliyuncs.com"))
                .unwrap();
        let printed = format!("{credential:?}");
        assert!(!printed.contains("sk-test"));
        assert!(printed.contains("redacted"));
    }
}
