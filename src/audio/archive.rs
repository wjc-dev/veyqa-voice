//! Audio archive plus the transcription cache with corruption-safe writes.
//! Mirrors `src/audio/archive.py`.

use std::fmt;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value};

use crate::paths;
use crate::timeutil;

pub const CACHE_FILENAME: &str = "cache.json";
const AUDIO_EXTENSIONS: [&str; 8] = [
    ".wav", ".mp3", ".m4a", ".flac", ".ogg", ".aac", ".webm", ".pcm",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptionCacheError(pub String);

impl fmt::Display for TranscriptionCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TranscriptionCacheError {}

pub struct AudioArchiveManager {
    archive_dir: PathBuf,
    audio_dir: PathBuf,
    cache_path: PathBuf,
    backup_path: PathBuf,
    // Queue worker and streaming callbacks live on different threads; the
    // cache read-modify-write must be serialized or records are lost.
    cache_lock: Mutex<()>,
}

impl AudioArchiveManager {
    pub fn new(archive_dir: Option<&Path>) -> Self {
        let archive_dir = archive_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(paths::audio_archive_dir);
        let audio_dir = archive_dir.join("audio");
        let cache_path = archive_dir.join(CACHE_FILENAME);
        let mut backup = cache_path.as_os_str().to_os_string();
        backup.push(".bak");
        let manager = Self {
            archive_dir,
            audio_dir,
            cache_path,
            backup_path: PathBuf::from(backup),
            cache_lock: Mutex::new(()),
        };
        manager.ensure_directory();
        manager
    }

    pub fn archive_dir(&self) -> &Path {
        &self.archive_dir
    }

    pub fn audio_dir(&self) -> &Path {
        &self.audio_dir
    }

    pub fn cache_path(&self) -> &Path {
        &self.cache_path
    }

    pub fn backup_path(&self) -> &Path {
        &self.backup_path
    }

    fn ensure_directory(&self) {
        let _ = fs::create_dir_all(&self.archive_dir);
        let _ = fs::create_dir_all(&self.audio_dir);
        self.migrate_legacy_archive_entries();
    }

    /// Older versions dumped recordings in the archive root; keep only audio
    /// files moving, never the cache, its backup, or user notes.
    fn migrate_legacy_archive_entries(&self) {
        let Ok(entries) = fs::read_dir(&self.archive_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let source = entry.path();
            if !source.is_file() {
                continue;
            }
            let Some(extension) = source.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            let extension = format!(".{}", extension.to_lowercase());
            if !AUDIO_EXTENSIONS.contains(&extension.as_str()) {
                continue;
            }
            let Some(filename) = source.file_name() else {
                continue;
            };
            let target = unique_path(&self.audio_dir, &filename.to_string_lossy());
            if fs::rename(&source, &target).is_err() && fs::copy(&source, &target).is_ok() {
                let _ = fs::remove_file(&source);
            }
        }
    }

    pub fn save_audio_bytes(&self, audio_bytes: &[u8], prefix: &str) -> Option<PathBuf> {
        if audio_bytes.is_empty() {
            return None;
        }
        self.ensure_directory();
        let base_name = format!("{prefix}_{}", timeutil::local_compact_timestamp());
        let mut archive_path = self.audio_dir.join(format!("{base_name}.wav"));
        let mut suffix = 1;
        while archive_path.exists() {
            archive_path = self.audio_dir.join(format!("{base_name}_{suffix}.wav"));
            suffix += 1;
        }
        fs::write(&archive_path, audio_bytes).ok()?;
        Some(archive_path)
    }

    /// Reading must never silently return an empty map, or the next save would
    /// wipe every historical record.
    pub fn load_transcription_cache(
        &self,
    ) -> Result<Map<String, Value>, TranscriptionCacheError> {
        if !self.cache_path.exists() {
            return Ok(Map::new());
        }
        let text = fs::read_to_string(&self.cache_path).map_err(|error| {
            TranscriptionCacheError(format!("读取转录缓存失败: {error}"))
        })?;
        let data: Value = match serde_json::from_str(&text) {
            Ok(data) => data,
            Err(error) => return self.handle_corrupt_cache(&error.to_string()),
        };
        match data {
            Value::Object(map) => Ok(map),
            other => self.handle_corrupt_cache(&format!(
                "顶层结构应为 dict，实际是 {}",
                python_type_name(&other)
            )),
        }
    }

    /// Quarantine the corrupt file first, then fall back to the previous
    /// snapshot before ever starting over.
    fn handle_corrupt_cache(
        &self,
        _reason: &str,
    ) -> Result<Map<String, Value>, TranscriptionCacheError> {
        let stamp = timeutil::local_compact_stamp();
        let quarantine = unique_path(
            &self.archive_dir,
            &format!("{CACHE_FILENAME}.corrupt-{stamp}"),
        );
        if let Err(error) = fs::rename(&self.cache_path, &quarantine) {
            return Err(TranscriptionCacheError(format!(
                "保留损坏的转录缓存失败: {error}"
            )));
        }
        if let Some(backup) = self.load_backup_cache() {
            return Ok(backup);
        }
        Ok(Map::new())
    }

    fn load_backup_cache(&self) -> Option<Map<String, Value>> {
        if !self.backup_path.exists() {
            return None;
        }
        let text = fs::read_to_string(&self.backup_path).ok()?;
        let data: Value = serde_json::from_str(&text).ok()?;
        data.as_object().cloned()
    }

    /// Hard-link the current cache to `.bak.new`, then move it into place so
    /// the backup keeps pointing at the previous inode after the atomic
    /// rename of the new cache.
    fn snapshot_current_cache(&self) {
        if !self.cache_path.exists() {
            return;
        }
        let mut link = self.backup_path.as_os_str().to_os_string();
        link.push(".new");
        let link_path = PathBuf::from(link);
        if link_path.exists() {
            let _ = fs::remove_file(&link_path);
        }
        if fs::hard_link(&self.cache_path, &link_path).is_err() {
            return;
        }
        let _ = fs::rename(&link_path, &self.backup_path);
    }

    pub fn save_transcription_cache(&self, cache_data: &Map<String, Value>) {
        self.ensure_directory();
        let mut tmp = self.cache_path.as_os_str().to_os_string();
        tmp.push(format!(".{}.tmp", std::process::id()));
        let tmp_path = PathBuf::from(tmp);

        let write = (|| -> std::io::Result<()> {
            let mut file = File::create(&tmp_path)?;
            let body = serde_json::to_string_pretty(cache_data)
                .map_err(std::io::Error::other)?;
            file.write_all(body.as_bytes())?;
            file.flush()?;
            file.sync_all()?;
            Ok(())
        })();

        if write.is_ok() {
            self.snapshot_current_cache();
            if fs::rename(&tmp_path, &self.cache_path).is_ok() {
                return;
            }
        }
        let _ = fs::remove_file(&tmp_path);
    }

    pub fn save_transcription_result(
        &self,
        archive_path: Option<&Path>,
        transcription_result: &str,
        service: &str,
        model: &str,
        mode: &str,
    ) {
        let Some(archive_path) = archive_path else {
            return;
        };
        if transcription_result.is_empty() {
            return;
        }
        let filename = archive_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();

        let _guard = self
            .cache_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Ok(mut cache) = self.load_transcription_cache() else {
            return;
        };
        let mut entry = Map::new();
        entry.insert(
            "transcription".into(),
            Value::String(transcription_result.to_string()),
        );
        entry.insert("service".into(), Value::String(service.to_string()));
        entry.insert("model".into(), Value::String(model.to_string()));
        entry.insert("mode".into(), Value::String(mode.to_string()));
        entry.insert(
            "timestamp".into(),
            Value::String(timeutil::local_iso_timestamp()),
        );
        cache.insert(filename, Value::Object(entry));
        self.save_transcription_cache(&cache);
    }
}

fn unique_path(directory: &Path, filename: &str) -> PathBuf {
    let candidate = directory.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| filename.to_string());
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().into_owned());
    let mut suffix = 1;
    loop {
        let name = match &extension {
            Some(extension) => format!("{stem}_{suffix}.{extension}"),
            None => format!("{stem}_{suffix}"),
        };
        let candidate = directory.join(name);
        if !candidate.exists() {
            return candidate;
        }
        suffix += 1;
    }
}

fn python_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) => {
            if number.as_i64().is_some() || number.as_u64().is_some() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_manager() -> AudioArchiveManager {
        let directory = std::env::temp_dir().join(format!(
            "veyqa-archive-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        AudioArchiveManager::new(Some(&directory))
    }

    fn original_cache() -> Map<String, Value> {
        let mut cache = Map::new();
        cache.insert(
            "old.wav".into(),
            serde_json::json!({ "transcription": "旧记录" }),
        );
        cache
    }

    #[test]
    fn cache_round_trip_is_valid_json() {
        let manager = temp_manager();
        let mut cache = Map::new();
        cache.insert(
            "a.wav".into(),
            serde_json::json!({ "transcription": "测试" }),
        );
        manager.save_transcription_cache(&cache);

        let loaded = manager.load_transcription_cache().unwrap();
        assert_eq!(loaded["a.wav"]["transcription"], "测试");
        let text = fs::read_to_string(manager.cache_path()).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["a.wav"]["transcription"], "测试");
    }

    #[test]
    fn corrupt_cache_is_quarantined_and_the_backup_is_used() {
        let manager = temp_manager();
        let original = original_cache();
        manager.save_transcription_cache(&original);
        let mut extended = original.clone();
        extended.insert(
            "new.wav".into(),
            serde_json::json!({ "transcription": "新记录" }),
        );
        manager.save_transcription_cache(&extended);

        fs::write(manager.cache_path(), "{\"broken\":").unwrap();
        let recovered = manager.load_transcription_cache().unwrap();
        assert_eq!(recovered, original);

        let quarantined: Vec<_> = fs::read_dir(manager.archive_dir())
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("cache.json.corrupt-")
            })
            .collect();
        assert_eq!(quarantined.len(), 1);
        assert_eq!(
            fs::read_to_string(quarantined[0].path()).unwrap(),
            "{\"broken\":"
        );
    }

    #[test]
    fn corrupt_cache_without_backup_starts_empty_but_keeps_the_quarantine() {
        let manager = temp_manager();
        fs::write(manager.cache_path(), "[]").unwrap();
        assert!(manager.load_transcription_cache().unwrap().is_empty());
        assert!(fs::read_dir(manager.archive_dir())
            .unwrap()
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("cache.json.corrupt-")));
    }

    #[test]
    fn concurrent_result_updates_do_not_drop_entries() {
        let manager = std::sync::Arc::new(temp_manager());
        let paths: Vec<PathBuf> = (0..20)
            .map(|index| {
                let path = manager.audio_dir().join(format!("{index}.wav"));
                fs::write(&path, b"wav").unwrap();
                path
            })
            .collect();

        let handles: Vec<_> = paths
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                let manager = std::sync::Arc::clone(&manager);
                std::thread::spawn(move || {
                    manager.save_transcription_result(
                        Some(&path),
                        &format!("text-{index}"),
                        "qwen",
                        "test",
                        "transcriptions",
                    );
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(manager.load_transcription_cache().unwrap().len(), 20);
    }

    #[test]
    fn migration_moves_only_audio_files() {
        let root = std::env::temp_dir().join(format!(
            "veyqa-archive-migrate-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("legacy.wav"), b"audio").unwrap();
        fs::write(root.join("legacy.WAV"), b"upper").unwrap();
        fs::write(root.join("notes.md"), "keep").unwrap();

        let manager = AudioArchiveManager::new(Some(&root));
        assert!(manager.audio_dir().join("legacy.wav").exists());
        assert!(manager.audio_dir().join("legacy.WAV").exists());
        assert!(root.join("notes.md").exists());
        assert!(!root.join("legacy.wav").exists());
    }

    #[test]
    fn audio_bytes_get_unique_wav_names_and_empty_input_is_ignored() {
        let manager = temp_manager();
        assert!(manager.save_audio_bytes(&[], "recording").is_none());

        let first = manager.save_audio_bytes(b"one", "recording").unwrap();
        let second = manager.save_audio_bytes(b"two", "recording").unwrap();
        assert_ne!(first, second);
        assert_eq!(first.extension().unwrap(), "wav");
        assert!(first.file_name().unwrap().to_string_lossy().starts_with("recording_"));
        assert!(second.file_name().unwrap().to_string_lossy().contains("_1.wav"));
    }

    #[test]
    fn result_entries_carry_metadata_and_skip_empty_inputs() {
        let manager = temp_manager();
        let audio = manager.audio_dir().join("clip.wav");
        fs::write(&audio, b"wav").unwrap();

        manager.save_transcription_result(None, "text", "qwen", "m", "transcriptions");
        manager.save_transcription_result(Some(&audio), "", "qwen", "m", "transcriptions");
        assert!(manager.load_transcription_cache().unwrap().is_empty());

        manager.save_transcription_result(
            Some(&audio),
            "文本",
            "qwen",
            "model-x",
            "transcriptions",
        );
        let cache = manager.load_transcription_cache().unwrap();
        let entry = &cache["clip.wav"];
        assert_eq!(entry["transcription"], "文本");
        assert_eq!(entry["service"], "qwen");
        assert_eq!(entry["model"], "model-x");
        assert_eq!(entry["mode"], "transcriptions");
        assert!(entry["timestamp"].as_str().unwrap().contains('T'));
    }
}
