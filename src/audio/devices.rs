use anyhow::{bail, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, Host, StreamConfig, SupportedStreamConfig};

pub const PREFERRED_DEVICE_KEYWORDS: [&str; 8] = [
    "dji mic",
    "wireless mic",
    "external microphone",
    "accentum",
    "macbook pro microphone",
    "macbook air microphone",
    "airpods",
    "麦克风",
];

pub fn preferred_keyword_index(name: &str) -> Option<usize> {
    let name = name.to_lowercase();
    PREFERRED_DEVICE_KEYWORDS
        .iter()
        .position(|keyword| name.contains(keyword))
}

pub fn list_input_devices(host: &Host) -> Result<Vec<(String, Device)>> {
    let mut devices = Vec::new();
    if let Ok(iter) = host.input_devices() {
        for device in iter {
            let name = device.description().map(|description| description.name().to_string()).unwrap_or_else(|_| "未知设备".to_string());
            devices.push((name, device));
        }
    }
    Ok(devices)
}

fn find_device<F>(host: &Host, predicate: F) -> Option<(String, Device)>
where
    F: Fn(&str) -> bool,
{
    let iter = host.input_devices().ok()?;
    for device in iter {
        let name = device.description().map(|description| description.name().to_string()).unwrap_or_default();
        if predicate(&name) {
            return Some((name, device));
        }
    }
    None
}

pub fn select_input_device(
    host: &Host,
    configured: &str,
    policy: &str,
) -> Result<(String, Device)> {
    let configured = configured.trim().to_lowercase();
    if !configured.is_empty() {
        if let Some(found) = find_device(host, |name| name.to_lowercase().contains(&configured)) {
            return Ok(found);
        }
    }

    if policy.trim().eq_ignore_ascii_case("preferred") {
        for keyword in PREFERRED_DEVICE_KEYWORDS {
            if let Some(found) = find_device(host, |name| name.to_lowercase().contains(keyword)) {
                return Ok(found);
            }
        }
    }

    if let Some(device) = host.default_input_device() {
        let name = device
            .description()
            .map(|description| description.name().to_string())
            .unwrap_or_else(|_| "系统默认输入".to_string());
        return Ok((name, device));
    }

    bail!("没有可用的音频输入设备")
}

pub fn preferred_sample_rate(device: &Device) -> u32 {
    let default_rate = device
        .default_input_config()
        .map(|config| config.sample_rate())
        .unwrap_or(16_000);
    if let Ok(configs) = device.supported_input_configs() {
        for config in configs {
            let min = config.min_sample_rate();
            let max = config.max_sample_rate();
            if (min..=max).contains(&16_000) {
                return 16_000;
            }
        }
    }
    default_rate
}

pub fn build_stream_config(device: &Device) -> Result<(StreamConfig, cpal::SampleFormat, u32)> {
    let supported: SupportedStreamConfig = device.default_input_config()?;
    let sample_format = supported.sample_format();
    let mut config = supported.config();
    let rate = preferred_sample_rate(device);
    config.sample_rate = rate;
    Ok((config, sample_format, rate))
}

pub fn describe(device: &Device) -> String {
    let default = device
        .default_input_config()
        .map(|config| {
            format!(
                "{}Hz/{}ch/{:?}",
                config.sample_rate(),
                config.channels(),
                config.sample_format()
            )
        })
        .unwrap_or_else(|_| "默认配置不可用".to_string());
    let mut rates: Vec<u32> = Vec::new();
    if let Ok(configs) = device.supported_input_configs() {
        for config in configs {
            let min = config.min_sample_rate();
            let max = config.max_sample_rate();
            if !rates.contains(&min) {
                rates.push(min);
            }
            if !rates.contains(&max) {
                rates.push(max);
            }
        }
    }
    rates.sort_unstable();
    format!("{default}; 支持采样率范围 {}", rates.first().zip(rates.last()).map(|(a, b)| format!("{a}..{b}")).unwrap_or_else(|| "未知".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_device_keywords_match_the_python_whitelist() {
        assert_eq!(PREFERRED_DEVICE_KEYWORDS.len(), 8);
        assert_eq!(preferred_keyword_index("DJI Mic 2"), Some(0));
        assert_eq!(preferred_keyword_index("Wireless Mic RX"), Some(1));
        assert_eq!(preferred_keyword_index("External Microphone"), Some(2));
        assert_eq!(preferred_keyword_index("Sennheiser ACCENTUM"), Some(3));
        assert_eq!(
            preferred_keyword_index("MacBook Pro Microphone"),
            Some(4)
        );
        assert_eq!(
            preferred_keyword_index("MacBook Air Microphone"),
            Some(5)
        );
        assert_eq!(preferred_keyword_index("John's AirPods Pro"), Some(6));
        assert_eq!(preferred_keyword_index("外接麦克风"), Some(7));
        assert_eq!(preferred_keyword_index("USB Audio"), None);
        // A bare "wireless" or "MacBook" match must not outrank the exact list.
        assert_eq!(preferred_keyword_index("Wireless Headphones"), None);
        assert_eq!(preferred_keyword_index("Some MacBook Speaker"), None);
    }
}
