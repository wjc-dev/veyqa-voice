//! Packaged release gates, shared with the native settings helper.
//! Mirrors `src/release_features.py`.

use std::sync::OnceLock;

use serde_json::{Map, Value};

use crate::paths;

static FLAGS: OnceLock<Map<String, Value>> = OnceLock::new();

fn load_flags() -> Map<String, Value> {
    for path in [
        paths::resource_root().join("release-features.json"),
        paths::resource_root().join("config").join("release-features.json"),
    ] {
        if path.is_file() {
            let Ok(text) = std::fs::read_to_string(&path) else {
                return Map::new();
            };
            return parse_flags(&text);
        }
    }
    Map::new()
}

fn parse_flags(text: &str) -> Map<String, Value> {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(flags)) => flags,
        _ => Map::new(),
    }
}

pub fn builtin_speech_enabled() -> bool {
    FLAGS
        .get_or_init(load_flags)
        .get("builtin_speech")
        == Some(&Value::Bool(true))
}

pub fn private_trial_enabled() -> bool {
    builtin_speech_enabled()
        && FLAGS
            .get_or_init(load_flags)
            .get("private_trial")
            == Some(&Value::Bool(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_literal_true_enables_the_gate() {
        let flags = parse_flags(
            r#"{"builtin_speech": true, "private_trial": true}"#,
        );
        assert_eq!(flags.get("builtin_speech"), Some(&Value::Bool(true)));
        assert_eq!(flags.get("private_trial"), Some(&Value::Bool(true)));

        let flags = parse_flags(r#"{"builtin_speech": "true"}"#);
        assert_ne!(flags.get("builtin_speech"), Some(&Value::Bool(true)));
    }

    #[test]
    fn malformed_or_non_object_flags_degrade_to_empty() {
        assert!(parse_flags("[]").is_empty());
        assert!(parse_flags("").is_empty());
        assert!(parse_flags("{").is_empty());
    }

    #[test]
    fn shipped_configuration_matches_the_private_distribution() {
        assert!(builtin_speech_enabled());
        assert!(private_trial_enabled());
    }
}
