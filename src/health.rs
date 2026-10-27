//! Bounded functional recovery: never infer failure from a quiet keyboard.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::diagnostics::DiagnosticsStore;
use crate::paths;

pub type Probe = Box<dyn FnMut(f64) -> Option<String> + Send>;
pub type Recover = Box<dyn FnMut(&str) + Send>;
pub type Clock = Box<dyn FnMut() -> f64 + Send>;

pub struct RuntimeHealth {
    probe: Probe,
    recover: Recover,
    clock: Clock,
    store: DiagnosticsStore,
    main_tick: f64,
    last_poll: f64,
    recovering: bool,
    clean_since: Option<f64>,
    last_write: f64,
    last_fault: Option<String>,
}

/// Python's recovery restarts the process, which re-arms the one-shot latch.
/// A dev process that skips `execv` must re-arm explicitly, or any stall after
/// the first fault is never recovered.
const REARM_AFTER_CLEAN_SECONDS: f64 = 10.0;

pub fn monotonic_clock() -> Clock {
    let start = Instant::now();
    Box::new(move || start.elapsed().as_secs_f64())
}

pub fn wall_clock() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default()
}

impl RuntimeHealth {
    pub fn new(probe: Probe, recover: Recover, path: Option<PathBuf>) -> Self {
        Self::with_clock(probe, recover, path, monotonic_clock())
    }

    pub fn with_clock(
        probe: Probe,
        recover: Recover,
        path: Option<PathBuf>,
        mut clock: Clock,
    ) -> Self {
        let now = clock();
        Self {
            probe,
            recover,
            clock,
            store: DiagnosticsStore::new(
                Some(path.unwrap_or_else(|| paths::data_dir().join("health.json"))),
                false,
            ),
            main_tick: now,
            last_poll: now,
            recovering: false,
            clean_since: None,
            last_write: f64::NEG_INFINITY,
            last_fault: None,
        }
    }

    pub fn health_path(&self) -> &std::path::Path {
        self.store.path()
    }

    pub fn tick_main(&mut self) {
        self.main_tick = (self.clock)();
    }

    pub fn poll(&mut self) -> Result<()> {
        let now = (self.clock)();
        // Give the UI and device services time to return after sleep/suspension.
        if now - self.last_poll > 10.0 {
            self.main_tick = now;
            self.last_poll = now;
            return Ok(());
        }
        self.last_poll = now;
        let mut reason = (self.probe)(now);
        if now - self.main_tick > 20.0 {
            reason = Some("应用事件循环无响应".to_string());
        }
        if now - self.last_write >= 10.0 || reason != self.last_fault {
            let mut values = Map::new();
            values.insert("pid".to_string(), json!(std::process::id()));
            values.insert("timestamp".to_string(), json!(wall_clock()));
            values.insert("fault".to_string(), json!(reason.clone().unwrap_or_default()));
            self.store.update(&values)?;
            self.last_write = now;
            self.last_fault = reason.clone();
        }
        match reason {
            Some(reason) => {
                self.clean_since = None;
                if !self.recovering {
                    self.recovering = true;
                    (self.recover)(&reason);
                }
            }
            None => {
                if self.recovering {
                    let since = *self.clean_since.get_or_insert(now);
                    if now - since >= REARM_AFTER_CLEAN_SECONDS {
                        self.recovering = false;
                        self.clean_since = None;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn run_forever(&mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs(2));
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let _ = self.poll();
        }
    }
}

/// Persist a rolling budget across restart; never loop indefinitely on a bad device.
pub fn allow_automatic_restart(path: Option<PathBuf>, now: Option<f64>) -> Result<bool> {
    let path = path.unwrap_or_else(|| paths::data_dir().join("automatic-restarts.json"));
    let now = now.unwrap_or_else(wall_clock);
    let entries: Vec<Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let mut recent: Vec<f64> = entries
        .iter()
        .filter_map(|entry| match entry {
            Value::Number(number) => number.as_f64(),
            Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
            _ => None,
        })
        .filter(|stamp| {
            let delta = now - stamp;
            (0.0..300.0).contains(&delta)
        })
        .collect();
    if recent.len() >= 3 {
        return Ok(false);
    }
    recent.push(now);
    DiagnosticsStore::new(Some(path), true).save(&json!(recent))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "veyqa-health-{name}-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn health_with(
        probe: Arc<AtomicUsize>,
        recovered: Arc<AtomicUsize>,
        times: Vec<f64>,
        path: PathBuf,
        reason: Option<&'static str>,
    ) -> RuntimeHealth {
        let mut ticks = times.into_iter();
        let clock: Clock = Box::new(move || ticks.next().unwrap_or(1000.0));
        let probe: Probe = Box::new(move |_now| {
            probe.fetch_add(1, Ordering::SeqCst);
            reason.map(str::to_string)
        });
        let recover: Recover = Box::new(move |_reason| {
            recovered.fetch_add(1, Ordering::SeqCst);
        });
        RuntimeHealth::with_clock(probe, recover, Some(path), clock)
    }

    #[test]
    fn recovery_rearms_after_a_clean_window() {
        let recovered = Arc::new(AtomicUsize::new(0));
        let path = temp_path("rearm");
        let mut ticks = vec![
            0.0, 2.0, 2.0, 8.0, 8.0, 12.0, 12.0, 20.0, 20.0, 21.0, 21.0, 22.0, 22.0, 30.0, 30.0,
            40.0, 40.0, 42.0, 42.0,
        ]
        .into_iter();
        let clock: Clock = Box::new(move || ticks.next().unwrap_or(1000.0));
        let probe: Probe = Box::new(|now| {
            let faulty = now < 1.0 || (8.0..10.0).contains(&now) || (22.0..23.0).contains(&now) || now >= 42.0;
            faulty.then(|| "识别结束超时".to_string())
        });
        let hits = recovered.clone();
        let recover: Recover = Box::new(move |_reason| {
            hits.fetch_add(1, Ordering::SeqCst);
        });
        let mut health = RuntimeHealth::with_clock(probe, recover, Some(path.clone()), clock);
        for _ in 0..9 {
            health.tick_main();
            health.poll().unwrap();
        }
        // One recovery at t=0; the 8-10s fault is too close to re-arm; only
        // after the 10s clean window ending at t=40 does t=42 recover again.
        assert_eq!(recovered.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn long_gap_after_sleep_resets_without_recovery() {
        let probe = Arc::new(AtomicUsize::new(0));
        let recovered = Arc::new(AtomicUsize::new(0));
        let path = temp_path("sleep");
        let mut health = health_with(
            probe.clone(),
            recovered.clone(),
            vec![0.0, 50.0],
            path.clone(),
            Some("设备无响应"),
        );
        health.poll().unwrap();
        assert_eq!(probe.load(Ordering::SeqCst), 0);
        assert_eq!(recovered.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stuck_main_thread_is_reported_even_when_probe_is_clean() {
        let probe = Arc::new(AtomicUsize::new(0));
        let recovered = Arc::new(AtomicUsize::new(0));
        let path = temp_path("stuck");
        let ticks: Vec<f64> = (0..=11).map(|index| index as f64 * 2.0).collect();
        let mut health = health_with(
            probe,
            recovered.clone(),
            ticks,
            path.clone(),
            None,
        );
        for _ in 0..11 {
            health.poll().unwrap();
        }
        assert_eq!(recovered.load(Ordering::SeqCst), 1);
        let data: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(data["fault"], json!("应用事件循环无响应"));
        assert_eq!(data["pid"], json!(std::process::id()));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn recovery_is_not_requested_twice_for_the_same_run() {
        let probe = Arc::new(AtomicUsize::new(0));
        let recovered = Arc::new(AtomicUsize::new(0));
        let path = temp_path("once");
        let mut health = health_with(
            probe,
            recovered.clone(),
            vec![0.0, 1.0, 2.0, 3.0],
            path.clone(),
            Some("键盘监听失效"),
        );
        for _ in 0..3 {
            health.poll().unwrap();
        }
        assert_eq!(recovered.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn restart_budget_allows_three_restarts_per_five_minutes() {
        let path = temp_path("budget");
        assert!(allow_automatic_restart(Some(path.clone()), Some(1000.0)).unwrap());
        assert!(allow_automatic_restart(Some(path.clone()), Some(1100.0)).unwrap());
        assert!(allow_automatic_restart(Some(path.clone()), Some(1200.0)).unwrap());
        assert!(!allow_automatic_restart(Some(path.clone()), Some(1299.0)).unwrap());
        assert!(allow_automatic_restart(Some(path.clone()), Some(1300.0)).unwrap());
        assert!(!allow_automatic_restart(Some(path.clone()), Some(1350.0)).unwrap());
        assert!(allow_automatic_restart(Some(path.clone()), Some(1601.0)).unwrap());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn corrupt_restart_budget_is_ignored() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "not json").unwrap();
        assert!(allow_automatic_restart(Some(path.clone()), Some(1000.0)).unwrap());
        let _ = std::fs::remove_file(path);
    }
}
