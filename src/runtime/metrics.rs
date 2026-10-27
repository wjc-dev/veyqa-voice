//! Content-free per-utterance latency log used by the acceptance benchmarks.
//!
//! Every line is appended to `logs/session-metrics.jsonl`; the text itself is
//! never recorded here (archive/diagnostics have their own opt-in channels).

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::OnceLock;
use std::time::Instant;

use serde_json::{json, Value};

use crate::paths;

const MAX_LOG_BYTES: u64 = 512 * 1024;

static PROCESS_START: OnceLock<Instant> = OnceLock::new();

/// Called once at process entry so the cold-start marker has an origin.
pub fn start_clock() {
    PROCESS_START.get_or_init(Instant::now);
}

/// Written once the menu bar is live and the AppKit loop is about to run.
pub fn append_startup() {
    let Some(start) = PROCESS_START.get() else {
        return;
    };
    append_to(
        "startup.jsonl",
        &json!({ "startup_ms": start.elapsed().as_millis() as u64 }),
    );
}

#[derive(Debug)]
pub struct SessionMetrics {
    session: String,
    key_down: f64,
    audio_ready: Option<f64>,
    first_partial: Option<f64>,
    key_up: Option<f64>,
    final_received: Option<f64>,
    pasted: Option<f64>,
}

impl SessionMetrics {
    pub fn new(session: String, key_down: f64) -> Self {
        Self {
            session,
            key_down,
            audio_ready: None,
            first_partial: None,
            key_up: None,
            final_received: None,
            pasted: None,
        }
    }

    pub fn mark_audio_ready(&mut self, at: f64) {
        self.audio_ready.get_or_insert(at);
    }

    pub fn mark_partial(&mut self, at: f64) {
        self.first_partial.get_or_insert(at);
    }

    pub fn mark_key_up(&mut self, at: f64) {
        self.key_up.get_or_insert(at);
    }

    pub fn mark_final(&mut self, at: f64, committed: bool) {
        self.final_received.get_or_insert(at);
        if committed {
            self.pasted.get_or_insert(at);
        }
    }

    pub fn received_final(&self) -> bool {
        self.final_received.is_some()
    }

    pub fn committed(&self) -> bool {
        self.pasted.is_some()
    }

    /// Deltas stay `null` when a phase never happened (cancel, error, quiet).
    pub fn snapshot(&self, outcome: &str) -> Value {
        let delta = |end: Option<f64>, start: f64| {
            end.map(|end| ((end - start) * 1000.0).round() as i64)
        };
        json!({
            "at": crate::timeutil::local_iso_timestamp(),
            "session": self.session,
            "outcome": outcome,
            "key_down_to_audio_ready_ms": delta(self.audio_ready, self.key_down),
            "key_down_to_first_partial_ms": delta(self.first_partial, self.key_down),
            "key_down_to_final_ms": delta(self.final_received, self.key_down),
            "key_up_to_final_ms": self.key_up.and_then(|up| delta(self.final_received, up)),
            "key_down_to_paste_ms": delta(self.pasted, self.key_down),
            "key_up_to_paste_ms": self.key_up.and_then(|up| delta(self.pasted, up)),
        })
    }
}

pub fn append(line: &Value) {
    append_to("session-metrics.jsonl", line);
}

fn append_to(name: &str, line: &Value) {
    let directory = paths::log_dir();
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    let path = directory.join(name);
    rotate(&path);
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
    else {
        return;
    };
    let _ = writeln!(file, "{line}");
}

fn rotate(path: &std::path::Path) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.len() < MAX_LOG_BYTES {
        return;
    }
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".1");
    let _ = std::fs::rename(path, backup);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_are_relative_to_key_down_and_key_up() {
        let mut metrics = SessionMetrics::new("s1".into(), 10.0);
        metrics.mark_audio_ready(10.03);
        metrics.mark_partial(10.5);
        metrics.mark_key_up(12.0);
        metrics.mark_final(12.4, true);
        let line = metrics.snapshot("committed");
        assert_eq!(line["key_down_to_audio_ready_ms"], json!(30));
        assert_eq!(line["key_down_to_first_partial_ms"], json!(500));
        assert_eq!(line["key_up_to_final_ms"], json!(400));
        assert_eq!(line["key_down_to_paste_ms"], json!(2400));
        assert_eq!(line["key_up_to_paste_ms"], json!(400));
    }

    #[test]
    fn missing_phases_stay_null() {
        let mut metrics = SessionMetrics::new("s2".into(), 1.0);
        metrics.mark_key_up(1.2);
        let line = metrics.snapshot("no_final");
        assert!(line["key_down_to_audio_ready_ms"].is_null());
        assert!(line["key_down_to_first_partial_ms"].is_null());
        assert!(line["key_down_to_final_ms"].is_null());
        assert!(line["key_up_to_paste_ms"].is_null());
    }

    #[test]
    fn uncommitted_final_has_no_paste_timestamp() {
        let mut metrics = SessionMetrics::new("s3".into(), 0.0);
        metrics.mark_final(1.0, false);
        let line = metrics.snapshot("not_committed");
        assert_eq!(line["key_down_to_final_ms"], json!(1000));
        assert!(line["key_down_to_paste_ms"].is_null());
    }
}
