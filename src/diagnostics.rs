//! Private non-content runtime diagnostics shared with the Swift settings app.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::paths;
use crate::timeutil::local_timestamp;

pub struct DiagnosticsStore {
    path: PathBuf,
    durable: bool,
    lock: Mutex<()>,
}

impl DiagnosticsStore {
    pub fn new(path: Option<PathBuf>, durable: bool) -> Self {
        Self {
            path: path.unwrap_or_else(paths::diagnostics_file),
            durable,
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn update(&self, values: &Map<String, Value>) -> Result<()> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut data = self.read_object();
        if values
            .iter()
            .all(|(key, value)| data.get(key) == Some(value))
        {
            return Ok(());
        }
        for (key, value) in values {
            data.insert(key.clone(), value.clone());
        }
        data.insert("updated_at".to_string(), Value::String(local_timestamp()));
        self.save(&Value::Object(data))
    }

    pub fn read(&self) -> Value {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        serde_json::from_str(&fs::read_to_string(&self.path).unwrap_or_default())
            .unwrap_or(Value::Null)
    }

    fn read_object(&self) -> Map<String, Value> {
        match fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        {
            Some(Value::Object(object)) => object,
            _ => Map::new(),
        }
    }

    /// Atomic private write used by the diagnostics and restart-budget files.
    pub(crate) fn save(&self, data: &Value) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建 {}", parent.display()))?;
        }
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "diagnostics.json".to_string());
        let temporary = parent.join(format!(".{name}.{}", uuid::Uuid::new_v4().simple()));

        let write = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .with_context(|| format!("无法创建 {}", temporary.display()))?;
            let mut body = serde_json::to_string_pretty(data)?;
            body.push('\n');
            file.write_all(body.as_bytes())?;
            if self.durable {
                file.sync_all()?;
            }
            fs::rename(&temporary, &self.path)
                .with_context(|| format!("无法替换 {}", self.path.display()))?;
            Ok(())
        })();
        if write.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "veyqa-diagnostics-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn merges_values_and_stamps_updated_at() {
        let path = temp_path();
        let store = DiagnosticsStore::new(Some(path.clone()), true);
        let mut first = Map::new();
        first.insert("status".to_string(), json!("idle"));
        store.update(&first).unwrap();

        let mut second = Map::new();
        second.insert("fault".to_string(), json!(""));
        store.update(&second).unwrap();

        let data: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(data["status"], json!("idle"));
        assert_eq!(data["fault"], json!(""));
        assert!(data["updated_at"].as_str().unwrap().contains('T'));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn unchanged_values_do_not_rewrite_the_file() {
        let path = temp_path();
        fs::write(
            &path,
            "{\n  \"status\": \"idle\",\n  \"updated_at\": \"2020-01-01T00:00:00+08:00\"\n}\n",
        )
        .unwrap();
        let store = DiagnosticsStore::new(Some(path.clone()), false);
        let mut values = Map::new();
        values.insert("status".to_string(), json!("idle"));
        store.update(&values).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("2020-01-01T00:00:00+08:00"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn invalid_content_is_replaced_by_a_fresh_object() {
        let path = temp_path();
        fs::write(&path, "[1, 2, 3]").unwrap();
        let store = DiagnosticsStore::new(Some(path.clone()), true);
        let mut values = Map::new();
        values.insert("pid".to_string(), json!(42));
        store.update(&values).unwrap();

        let data: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(data["pid"], json!(42));
        let _ = fs::remove_file(path);
    }
}
