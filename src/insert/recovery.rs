use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};

use crate::paths;
use crate::timeutil::local_timestamp;

pub type RecoveryEntry = Map<String, Value>;

const REQUIRED_FIELDS: [&str; 4] = ["id", "text", "reason", "timestamp"];

fn default_path() -> PathBuf {
    let configured = std::env::var("RECOVERY_STORE_FILE").unwrap_or_default();
    let configured = configured.trim();
    if !configured.is_empty() {
        return paths::expand_path(configured);
    }
    paths::recovery_file()
}

fn valid_entries(value: &Value) -> Option<Vec<RecoveryEntry>> {
    let array = value.as_array()?;
    let mut entries = Vec::with_capacity(array.len());
    for item in array {
        let object = item.as_object()?;
        if !REQUIRED_FIELDS
            .iter()
            .all(|field| object.get(*field).is_some_and(Value::is_string))
        {
            return None;
        }
        entries.push(object.clone());
    }
    Some(entries)
}

struct StoreLock {
    fd: libc::c_int,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

fn acquire_file_lock(path: &Path) -> Result<StoreLock> {
    let bytes = path.as_os_str().as_bytes();
    let c_path = CString::new(bytes).context("恢复记录锁路径包含 NUL")?;
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_CREAT | libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("无法打开恢复记录锁 {}", path.display()));
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(StoreLock { fd });
        }
        let error = io::Error::last_os_error();
        let would_block = error.raw_os_error() == Some(libc::EWOULDBLOCK);
        if !would_block {
            unsafe { libc::close(fd) };
            return Err(error).context("无法锁定恢复记录");
        }
        if Instant::now() >= deadline {
            unsafe { libc::close(fd) };
            bail!("恢复记录正在更新，请重试");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub struct RecoveryStore {
    path: PathBuf,
    limit: usize,
    lock: Mutex<()>,
}

impl RecoveryStore {
    pub fn new(path: impl Into<PathBuf>, limit: usize) -> Self {
        Self {
            path: path.into(),
            limit: limit.clamp(1, 20),
            lock: Mutex::new(()),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(default_path(), 5)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Vec<RecoveryEntry> {
        let Ok(_guard) = self.lock.lock() else {
            return Vec::new();
        };
        self.read_entries(false).unwrap_or_default()
    }

    pub fn load_strict(&self) -> Result<Vec<RecoveryEntry>> {
        let _guard = self.lock.lock().map_err(|_| anyhow::anyhow!("恢复记录锁已中毒"))?;
        self.read_entries(true)
    }

    fn read_entries(&self, strict: bool) -> Result<Vec<RecoveryEntry>> {
        match fs::read(&self.path) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|value| valid_entries(&value))
            {
                Some(mut entries) => {
                    entries.truncate(self.limit);
                    Ok(entries)
                }
                None => {
                    if strict {
                        bail!("恢复记录格式无效，原文件保持不变");
                    }
                    Ok(Vec::new())
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => {
                if strict {
                    Err(error).with_context(|| format!("无法读取 {}", self.path.display()))
                } else {
                    Ok(Vec::new())
                }
            }
        }
    }

    pub fn add(&self, text: &str, reason: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let _guard = self.lock.lock().map_err(|_| anyhow::anyhow!("恢复记录锁已中毒"))?;
        let _file_lock = self.transaction_lock()?;
        let mut entries = self.read_entries(true)?;

        let mut entry = RecoveryEntry::new();
        entry.insert("id".to_string(), Value::String(uuid::Uuid::new_v4().simple().to_string()));
        entry.insert("text".to_string(), Value::String(text.to_string()));
        entry.insert("reason".to_string(), Value::String(reason.to_string()));
        entry.insert("timestamp".to_string(), Value::String(local_timestamp()));
        entries.insert(0, entry);
        entries.truncate(self.limit);
        self.save(&entries)
    }

    pub fn delete(&self, entry_id: &str) -> Result<bool> {
        let ids = HashSet::from([entry_id.to_string()]);
        self.delete_many(&ids)
    }

    pub fn delete_many(&self, entry_ids: &HashSet<String>) -> Result<bool> {
        let _guard = self.lock.lock().map_err(|_| anyhow::anyhow!("恢复记录锁已中毒"))?;
        let _file_lock = self.transaction_lock()?;
        let entries = self.read_entries(true)?;
        let original_len = entries.len();
        let updated: Vec<RecoveryEntry> = entries
            .into_iter()
            .filter(|entry| {
                entry
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(|id| !entry_ids.contains(id))
            })
            .collect();
        if updated.len() == original_len {
            return Ok(false);
        }
        self.save(&updated)?;
        Ok(true)
    }

    pub fn clear(&self) -> Result<()> {
        let _guard = self.lock.lock().map_err(|_| anyhow::anyhow!("恢复记录锁已中毒"))?;
        let _file_lock = self.transaction_lock()?;
        self.save(&[])
    }

    fn transaction_lock(&self) -> Result<StoreLock> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建 {}", parent.display()))?;
        }
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "recovery.json".to_string());
        let lock_path = self.path.with_file_name(format!(".{name}.lock"));
        acquire_file_lock(&lock_path)
    }

    fn save(&self, entries: &[RecoveryEntry]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建 {}", parent.display()))?;
        }
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "recovery.json".to_string());
        let temporary = self.path.with_file_name(format!(
            ".{name}.{}",
            uuid::Uuid::new_v4().simple()
        ));

        let write_result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .with_context(|| format!("无法创建 {}", temporary.display()))?;
            let serialized = serde_json::to_string_pretty(entries)?;
            file.write_all(serialized.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)
                .with_context(|| format!("无法替换 {}", self.path.display()))?;
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "veyqa-recovery-test-{}",
                uuid::Uuid::new_v4().simple()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join("recovery.json")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn texts(entries: &[RecoveryEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| entry["text"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn invalid_existing_file_is_not_overwritten() {
        let dir = TestDir::new();
        let path = dir.file();
        fs::write(&path, r#"{"interrupted":"#).unwrap();
        let store = RecoveryStore::new(&path, 5);
        assert!(store.add("新文字", "失败").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"interrupted":"#);
        assert!(store.delete("old").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"interrupted":"#);
    }

    #[test]
    fn keeps_only_the_latest_private_results() {
        let dir = TestDir::new();
        let path = dir.file();
        let store = RecoveryStore::new(&path, 2);
        store.add("第一条", "失焦").unwrap();
        store.add("第二条", "失败").unwrap();
        store.add("第三条", "失败").unwrap();

        assert_eq!(texts(&store.load()), ["第三条", "第二条"]);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn delete_and_clear() {
        let dir = TestDir::new();
        let path = dir.file();
        let store = RecoveryStore::new(&path, 5);
        store.add("可恢复文字", "失焦").unwrap();
        let entry_id = store.load()[0]["id"].as_str().unwrap().to_string();
        assert!(store.delete(&entry_id).unwrap());
        assert!(store.load().is_empty());
        store.add("再次失败", "失败").unwrap();
        store.clear().unwrap();
        assert!(store.load().is_empty());
    }

    #[test]
    fn delete_selected_ids_preserves_newer_unseen_text() {
        let dir = TestDir::new();
        let path = dir.file();
        let store = RecoveryStore::new(&path, 5);
        store.add("旧文字", "失败").unwrap();
        let selected = HashSet::from([store.load()[0]["id"].as_str().unwrap().to_string()]);
        store.add("刚刚恢复的新文字", "失败").unwrap();
        store.delete_many(&selected).unwrap();
        assert_eq!(texts(&store.load()), ["刚刚恢复的新文字"]);
    }

    #[test]
    fn separate_writers_serialize_read_modify_write() {
        let dir = TestDir::new();
        let path = dir.file();
        let first = Arc::new(RecoveryStore::new(&path, 20));
        let second = Arc::new(RecoveryStore::new(&path, 20));
        let mut handles = Vec::new();
        for (store, prefix) in [(first.clone(), "第一条"), (second.clone(), "第二条")] {
            handles.push(std::thread::spawn(move || {
                for index in 0..10 {
                    store.add(&format!("{prefix}-{index}"), "测试").unwrap();
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        let entries = first.load();
        assert_eq!(entries.len(), 20);
        let ids: HashSet<&str> = entries
            .iter()
            .map(|entry| entry["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 20);
    }

    #[test]
    fn keeps_extra_fields_when_rewriting() {
        let dir = TestDir::new();
        let path = dir.file();
        fs::write(
            &path,
            r#"[{"id":"a","text":"旧","reason":"失败","timestamp":"2026-01-01T00:00:00+08:00","extra":"keep"}]"#,
        )
        .unwrap();
        let store = RecoveryStore::new(&path, 5);
        store.add("新", "失败").unwrap();
        let entries = store.load();
        assert_eq!(entries[1]["extra"], "keep");
    }

    #[test]
    fn timestamp_is_iso_8601_with_offset() {
        let stamp = local_timestamp();
        assert_eq!(stamp.len(), 25, "{stamp}");
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[10..11], "T");
        assert!(stamp[19..20] == *"+" || stamp[19..20] == *"-");
        assert_eq!(&stamp[22..23], ":");
    }

    #[test]
    fn load_strict_reports_invalid_files() {
        let dir = TestDir::new();
        let path = dir.file();
        fs::write(&path, "[] trailing").unwrap();
        let store = RecoveryStore::new(&path, 5);
        assert!(store.load_strict().is_err());
        assert!(store.load().is_empty());
    }
}
