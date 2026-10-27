//! Configured ASR identifiers, independent of the provider's model catalogue.
//! Mirrors `src/transcription/speech_models.py`.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::paths;

const QWEN_PATTERN: &str =
    r"qwen-audio-\d+(?:\.\d+)*-asr-flash-(?:streaming|message)(?:-\d{4}-\d{2}-\d{2})?";
const DOUBAO_PATTERN: &str = r"volc\.[a-z0-9]+\.sauc\.(?:duration|concurrent)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    CatalogMissing,
    UnsupportedModel,
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            ModelError::CatalogMissing => "语音模型目录缺失，请重新安装应用",
            ModelError::UnsupportedModel => "请选择支持实时听写的语音模型",
        })
    }
}

impl std::error::Error for ModelError {}

#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    defaults: HashMap<String, String>,
}

impl ModelCatalog {
    pub fn default_model(&self, provider: &str) -> Option<&str> {
        self.defaults.get(provider).map(String::as_str)
    }
}

/// `RESOURCE_ROOT/asr-models.json` first, then the source checkout's copy.
pub fn catalog() -> Result<ModelCatalog, ModelError> {
    let paths = [
        paths::resource_root().join("asr-models.json"),
        paths::source_root().join("config/asr-models.json"),
    ];
    catalog_from(&paths)
}

fn catalog_from(candidates: &[PathBuf]) -> Result<ModelCatalog, ModelError> {
    for path in candidates {
        if path.is_file() {
            return parse_catalog(path);
        }
    }
    Err(ModelError::CatalogMissing)
}

fn parse_catalog(path: &Path) -> Result<ModelCatalog, ModelError> {
    let text = std::fs::read_to_string(path).map_err(|_| ModelError::CatalogMissing)?;
    let data: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| ModelError::CatalogMissing)?;
    let defaults = data
        .get("defaults")
        .and_then(serde_json::Value::as_object)
        .ok_or(ModelError::CatalogMissing)?;
    let mut catalog = ModelCatalog::default();
    for (provider, model) in defaults {
        let Some(model) = model.as_str() else {
            continue;
        };
        catalog.defaults.insert(provider.clone(), model.to_string());
    }
    Ok(catalog)
}

fn qwen_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(QWEN_PATTERN).expect("valid qwen model pattern"))
}

fn doubao_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(DOUBAO_PATTERN).expect("valid doubao model pattern"))
}

/// A refresh must never change a saved choice; only the pattern gates it.
pub fn selected_model(provider: &str, value: &str) -> Result<String, ModelError> {
    let catalog = catalog()?;
    let fallback = catalog
        .default_model(provider)
        .ok_or(ModelError::UnsupportedModel)?;
    let trimmed = value.trim();
    let model = if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    };
    let pattern = match provider {
        "qwen" => qwen_pattern(),
        "doubao" => doubao_pattern(),
        _ => return Err(ModelError::UnsupportedModel),
    };
    let matches = pattern
        .find(&model)
        .map(|found| found.start() == 0 && found.end() == model.len())
        .unwrap_or(false);
    if matches {
        Ok(model)
    } else {
        Err(ModelError::UnsupportedModel)
    }
}

/// Offline default for the bundled promotional service.
pub fn promotion_model() -> Result<String, ModelError> {
    catalog()?
        .default_model("promotion")
        .map(str::to_string)
        .ok_or(ModelError::UnsupportedModel)
}

/// Offline defaults from the shipped catalogue.
pub fn default_models() -> Result<HashMap<String, String>, ModelError> {
    Ok(catalog()?.defaults)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_catalogue_has_the_expected_offline_defaults() {
        let catalog = catalog().unwrap();
        assert_eq!(
            catalog.default_model("qwen"),
            Some("qwen-audio-3.0-asr-flash-streaming")
        );
        assert_eq!(catalog.default_model("doubao"), Some("volc.seedasr.sauc.duration"));
        assert_eq!(
            catalog.default_model("promotion"),
            Some("qwen-audio-3.1-asr-flash-message")
        );
    }

    #[test]
    fn blank_value_uses_the_offline_default_and_trimmed_values_pass_validation() {
        assert_eq!(
            selected_model("qwen", "").unwrap(),
            "qwen-audio-3.0-asr-flash-streaming"
        );
        assert_eq!(
            selected_model("qwen", "  ").unwrap(),
            "qwen-audio-3.0-asr-flash-streaming"
        );
        assert_eq!(
            selected_model("doubao", " volc.seedasr.sauc.concurrent ").unwrap(),
            "volc.seedasr.sauc.concurrent"
        );
    }

    #[test]
    fn accepts_supported_qwen_and_doubao_families() {
        for model in [
            "qwen-audio-3.1-asr-flash-message",
            "qwen-audio-3.1-asr-flash-streaming-2025-09-01",
            "qwen-audio-3.0-asr-flash-streaming",
            "qwen-audio-12.34-asr-flash-streaming",
        ] {
            assert!(selected_model("qwen", model).is_ok(), "{model}");
        }
        for model in [
            "volc.seedasr.sauc.duration",
            "volc.bigasr.sauc.concurrent",
        ] {
            assert!(selected_model("doubao", model).is_ok(), "{model}");
        }
    }

    #[test]
    fn rejects_other_models_and_unknown_providers() {
        for model in [
            "qwen-turbo",
            "qwen-audio-3.0-asr-flash",
            "qwen-audio-3.0-asr-flash-streaming \"x\"",
            "volc.seedasr.sauc",
            "volc.seedasr.sauc.durationx",
        ] {
            assert_eq!(
                selected_model("qwen", model),
                Err(ModelError::UnsupportedModel),
                "{model}"
            );
        }
        assert_eq!(
            selected_model("custom", "volc.seedasr.sauc.duration"),
            Err(ModelError::UnsupportedModel)
        );
    }

    #[test]
    fn missing_or_malformed_catalogue_reports_install_hint() {
        let directory = std::env::temp_dir().join(format!(
            "veyqa-models-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let missing = directory.join("missing.json");
        assert_eq!(
            catalog_from(&[missing]).unwrap_err(),
            ModelError::CatalogMissing
        );
        assert_eq!(
            ModelError::CatalogMissing.to_string(),
            "语音模型目录缺失，请重新安装应用"
        );

        let malformed = directory.join("asr-models.json");
        std::fs::write(&malformed, "{\"models\": []}").unwrap();
        assert_eq!(
            catalog_from(&[malformed]).unwrap_err(),
            ModelError::CatalogMissing
        );
    }
}
