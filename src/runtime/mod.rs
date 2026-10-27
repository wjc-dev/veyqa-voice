//! Production wiring: hotkey -> engine actor -> providers -> AX insertion.
//!
//! AppKit stays on the main thread; this module never touches it directly.

pub mod actor;
pub mod app;
pub mod host;
pub mod hub;
pub mod metrics;
pub mod sink;

use std::sync::OnceLock;
use std::time::Instant;

/// Monotonic seconds shared by sessions, health, and diagnostics.
pub fn clock() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

pub fn wall_clock() -> f64 {
    crate::health::wall_clock()
}

/// Commands accepted by the single engine-actor thread.
#[derive(Clone, Debug, PartialEq)]
pub enum EngineCommand {
    /// `at` is the hotkey-thread timestamp so latency excludes queueing.
    Start { generation: u64, at: f64 },
    Stop { at: f64 },
    Cancel,
    /// The health tick found a stuck transition or session.
    Recover { reason: String },
    /// The settings helper saved a new `.env`; reload mirrors and config.
    ReloadSettings,
    Shutdown,
}
