//! OpenAI-compatible speech transcription, submitted when recording ends.
//! Pure validation/WAV assembly from `src/transcription/custom_asr.py`; the
//! HTTP submission is wired up with the engine in M2.

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

use url::{Host, Position, Url};

pub const MAX_AUDIO_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptionUrlError;

impl fmt::Display for TranscriptionUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("请填写 HTTPS API 地址；本机服务可使用 HTTP")
    }
}

impl std::error::Error for TranscriptionUrlError {}

/// Accept HTTPS providers and loopback HTTP, without embedded credentials.
pub fn transcription_url(base: &str) -> Result<String, TranscriptionUrlError> {
    let normalized = base.trim();
    if normalized
        .chars()
        .any(|character| character.is_whitespace() || (character as u32) < 32)
    {
        return Err(TranscriptionUrlError);
    }
    let parsed = Url::parse(normalized).map_err(|_| TranscriptionUrlError)?;
    let local = is_loopback_host(parsed.host());
    if parsed.host().is_none()
        || has_userinfo(&parsed)
        || parsed.query().is_some_and(|query| !query.is_empty())
        || parsed.fragment().is_some_and(|fragment| !fragment.is_empty())
        || !(parsed.scheme() == "https"
            || (parsed.scheme() == "http" && local))
        || parsed.port() == Some(0)
    {
        return Err(TranscriptionUrlError);
    }
    let mut path = parsed.path().trim_end_matches('/').to_string();
    if !path.ends_with("/audio/transcriptions") {
        path.push_str("/audio/transcriptions");
    }
    let authority = &parsed[Position::BeforeHost..Position::AfterPort];
    Ok(format!("{}://{}{}", parsed.scheme(), authority, path))
}

fn has_userinfo(url: &Url) -> bool {
    !url.username().is_empty()
        || url.password().is_some()
        || url[..Position::BeforeHost].contains('@')
}

fn is_loopback_host(host: Option<Host<&str>>) -> bool {
    match host {
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address == Ipv4Addr::LOCALHOST,
        Some(Host::Ipv6(address)) => address == Ipv6Addr::LOCALHOST,
        None => false,
    }
}

#[derive(Debug, Clone, Default)]
pub struct CustomAsrSettings {
    pub base_url: String,
    pub api_key: String,
    pub model_name: String,
}

impl CustomAsrSettings {
    pub fn from_environment() -> Self {
        Self {
            base_url: env_value("CUSTOM_ASR_BASE_URL"),
            api_key: env_value("CUSTOM_ASR_API_KEY"),
            model_name: env_value("CUSTOM_ASR_MODEL"),
        }
    }

    pub fn is_available(&self) -> bool {
        let Ok(url) = transcription_url(&self.base_url) else {
            return false;
        };
        let local = Url::parse(&url)
            .map(|parsed| is_loopback_host(parsed.host()))
            .unwrap_or(false);
        !self.model_name.is_empty()
            && !self.model_name.chars().any(char::is_whitespace)
            && (!self.api_key.is_empty() || local)
            && !self.api_key.chars().any(char::is_whitespace)
    }
}

fn env_value(name: &str) -> String {
    std::env::var(name).unwrap_or_default().trim().to_string()
}

/// 16-bit mono PCM wrapped in the same 44-byte RIFF header Python's `wave`
/// module writes.
pub fn wav_bytes(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let byte_rate = sample_rate * 2;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_the_transcriptions_path_once() {
        assert_eq!(
            transcription_url("https://example.com/v1/").unwrap(),
            "https://example.com/v1/audio/transcriptions"
        );
        assert_eq!(
            transcription_url("https://example.com/v1/audio/transcriptions").unwrap(),
            "https://example.com/v1/audio/transcriptions"
        );
        assert_eq!(
            transcription_url("https://example.com/v1/audio/transcriptions///").unwrap(),
            "https://example.com/v1/audio/transcriptions"
        );
        assert_eq!(
            transcription_url("https://example.com").unwrap(),
            "https://example.com/audio/transcriptions"
        );
    }

    #[test]
    fn allows_loopback_http_including_ipv6() {
        assert_eq!(
            transcription_url("http://[::1]:8080/v1").unwrap(),
            "http://[::1]:8080/v1/audio/transcriptions"
        );
        assert_eq!(
            transcription_url("http://127.0.0.1:8000/v1").unwrap(),
            "http://127.0.0.1:8000/v1/audio/transcriptions"
        );
        assert_eq!(
            transcription_url("http://LOCALHOST/v1").unwrap(),
            "http://localhost/v1/audio/transcriptions"
        );
    }

    #[test]
    fn rejects_insecure_remote_and_embedded_credentials() {
        for url in [
            "http://example.com/v1",
            "https://key@example.com",
            "https://user:pass@example.com",
            "https://example.com?token=x",
            "https://example.com/v1#fragment",
            "file:///tmp",
            "ftp://example.com/v1",
            "https://example.com /v1",
            "https://example.com/\u{0}",
            "http://127.0.0.1:0/v1",
        ] {
            assert_eq!(
                transcription_url(url),
                Err(TranscriptionUrlError),
                "{url}"
            );
        }
        assert_eq!(
            transcription_url("https://[::2]/v1").unwrap(),
            "https://[::2]/v1/audio/transcriptions"
        );
    }

    #[test]
    fn user_facing_error_matches_the_settings_copy() {
        assert_eq!(
            TranscriptionUrlError.to_string(),
            "请填写 HTTPS API 地址；本机服务可使用 HTTP"
        );
    }

    #[test]
    fn availability_requires_model_and_key_outside_loopback() {
        let local = CustomAsrSettings {
            base_url: "http://127.0.0.1:8000/v1".into(),
            api_key: String::new(),
            model_name: "test-asr".into(),
        };
        assert!(local.is_available());

        assert!(!CustomAsrSettings {
            model_name: String::new(),
            ..local.clone()
        }
        .is_available());

        assert!(!CustomAsrSettings {
            base_url: "https://api.example.com/v1".into(),
            ..local.clone()
        }
        .is_available());

        assert!(!CustomAsrSettings {
            api_key: "has space".into(),
            ..local.clone()
        }
        .is_available());

        assert!(!CustomAsrSettings {
            model_name: "bad model".into(),
            ..local.clone()
        }
        .is_available());

        assert!(!CustomAsrSettings {
            base_url: "http://example.com/v1".into(),
            api_key: "key".into(),
            model_name: "test-asr".into(),
        }
        .is_available());
    }

    #[test]
    fn wav_header_matches_a_standard_16_bit_mono_riff_file() {
        let pcm = vec![0x01u8, 0x00].repeat(1600);
        let bytes = wav_bytes(&pcm, 16_000);
        let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(reader.samples::<i16>().count(), 1600);
    }

    #[test]
    fn wav_header_reports_exact_byte_counts() {
        let pcm = vec![0u8; 320];
        let bytes = wav_bytes(&pcm, 16_000);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36 + 320);
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 320);
        assert_eq!(bytes.len(), 44 + 320);
    }
}
