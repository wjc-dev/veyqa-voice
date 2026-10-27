//! Launching and reattaching the Swift settings helper.
//!
//! Mirrors `_launch_settings_window` in `src/ui/status_bar.py`. The helper is a
//! separate process with its own TCC identity; the engine only spawns it, hands
//! over the data directory through `VOICE_INPUT_*`, and raises it with SIGUSR1.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::paths;

pub const LOG_MAX_BYTES: u64 = 512 * 1024;
pub const HELPER_BUNDLE_NAME: &str = "VeyqaSettings";
pub const HELPER_EXECUTABLE_NAME: &str = "VeyqaSettings";
pub const READY_MARKER: &str = ".settings-instance";

/// Resolve the helper binary the app bundle was packaged with.
pub fn settings_binary() -> Option<PathBuf> {
    if let Some(override_path) = std::env::var_os("VEYQA_SETTINGS_BINARY") {
        let path = PathBuf::from(override_path);
        return path.is_file().then_some(path);
    }
    if let Some(bundle) = paths::app_bundle_path() {
        let bundled = bundle
            .join("Contents/Helpers")
            .join(format!("{HELPER_BUNDLE_NAME}.app"))
            .join("Contents/MacOS")
            .join(HELPER_EXECUTABLE_NAME);
        if bundled.is_file() {
            return Some(bundled);
        }
    }
    let dev = paths::resource_root()
        .join("build")
        .join(format!("{HELPER_BUNDLE_NAME}.app"))
        .join("Contents/MacOS")
        .join(HELPER_EXECUTABLE_NAME);
    dev.is_file().then_some(dev)
}

pub fn settings_log_path() -> PathBuf {
    paths::log_dir().join("settings_ui.log")
}

fn log_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    format!("{seconds}")
}

/// Append one line, rotating at 512 KiB exactly like the Python helper log.
pub fn write_status_log(message: &str) {
    write_status_log_to(&settings_log_path(), message);
}

fn write_status_log_to(path: &Path, message: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::metadata(path).map(|meta| meta.len()).unwrap_or(0) >= LOG_MAX_BYTES {
        let mut rotated = path.as_os_str().to_owned();
        rotated.push(".1");
        let rotated = PathBuf::from(rotated);
        let _ = fs::remove_file(&rotated);
        let _ = fs::rename(path, &rotated);
    }
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
    {
        let _ = writeln!(file, "{} {message}", log_timestamp());
    }
}

pub fn instance_pid() -> Option<libc::pid_t> {
    let text = fs::read_to_string(paths::settings_instance_file()).ok()?;
    let pid = text.trim().parse::<libc::pid_t>().ok()?;
    (pid > 1).then_some(pid)
}

pub fn process_alive(pid: libc::pid_t) -> bool {
    pid > 1 && unsafe { libc::kill(pid, 0) } == 0
}

/// Raise an already running helper. SIGUSR1 is installed by its app delegate.
pub fn signal_settings(pid: libc::pid_t) -> bool {
    process_alive(pid) && unsafe { libc::kill(pid, libc::SIGUSR1) } == 0
}

extern "C" {
    fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, size: u32) -> libc::c_int;
}

/// Actual executable behind a pid, used to prove the instance file belongs to
/// our helper before signaling it.
pub fn process_executable(pid: libc::pid_t) -> Option<PathBuf> {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let read = unsafe {
        proc_pidpath(
            pid,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    if read <= 0 {
        return None;
    }
    let path = String::from_utf8_lossy(&buffer[..read as usize]).to_string();
    Some(PathBuf::from(path))
}

fn same_executable(pid: libc::pid_t, expected: &Path) -> bool {
    let Some(actual) = process_executable(pid) else {
        return false;
    };
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical(&actual) == canonical(expected)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// A helper we started earlier is still alive.
    Reused,
    /// An orphaned helper (engine restart) answered SIGUSR1.
    Reattached,
    /// A fresh helper process was spawned.
    Spawned,
}

pub struct SettingsLauncher {
    child: Option<Child>,
}

impl Default for SettingsLauncher {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsLauncher {
    pub fn new() -> Self {
        Self { child: None }
    }

    /// Open (or raise) the settings window. `None` when no helper is packaged.
    pub fn open(&mut self) -> Result<Option<(libc::pid_t, LaunchOutcome)>> {
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(None) => {
                    let pid = child.id() as libc::pid_t;
                    write_status_log(&format!("reused settings process pid={pid}"));
                    self.activate(pid);
                    return Ok(Some((pid, LaunchOutcome::Reused)));
                }
                Ok(Some(status)) => {
                    write_status_log(&format!(
                        "settings process exited status={}",
                        status.code().unwrap_or(-1)
                    ));
                    self.child = None;
                }
                Err(_) => self.child = None,
            }
        }

        let Some(binary) = settings_binary() else {
            write_status_log("settings helper is not packaged");
            anyhow::bail!("没有找到设置窗口程序，请先完成应用打包");
        };

        if let Some(pid) = instance_pid() {
            if same_executable(pid, &binary) && signal_settings(pid) {
                write_status_log(&format!("reattached settings process pid={pid}"));
                self.activate(pid);
                return Ok(Some((pid, LaunchOutcome::Reattached)));
            }
        }

        let pid = self.spawn(&binary)?;
        Ok(Some((pid, LaunchOutcome::Spawned)))
    }

    fn spawn(&mut self, binary: &Path) -> Result<libc::pid_t> {
        let root = paths::resource_root();
        write_status_log(&format!("launch requested executable={}", binary.display()));
        let log_path = settings_log_path();
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建日志目录 {}", parent.display()))?;
        }
        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)
            .with_context(|| format!("无法打开 {}", log_path.display()))?;
        let stderr = stdout
            .try_clone()
            .with_context(|| format!("无法打开 {}", log_path.display()))?;

        let mut command = Command::new(binary);
        command
            .current_dir(&root)
            .env("VOICE_INPUT_ROOT", &root)
            .env("VOICE_INPUT_DATA_ROOT", paths::data_dir())
            .env("VOICE_INPUT_BUNDLED", if paths::is_frozen() { "true" } else { "false" })
            .env("VOICE_INPUT_PARENT_PID", std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        if let Some(bundle) = paths::app_bundle_path() {
            command.env("VOICE_INPUT_APP_PATH", bundle);
        }
        // start_new_session=True: the helper outlives terminal signals aimed at
        // this process group.
        command.process_group(0);

        let child = command
            .spawn()
            .with_context(|| format!("无法启动设置窗口 {}", binary.display()))?;
        let pid = child.id() as libc::pid_t;
        write_status_log(&format!("settings process started pid={pid}"));
        self.child = Some(child);
        Ok(pid)
    }

    /// Raise and focus the helper. Called from the menu-bar thread only.
    #[allow(deprecated)] // ActivateIgnoringOtherApps is a no-op on macOS 14+, still harmless.
    fn activate(&self, pid: libc::pid_t) {
        if pid <= 1 {
            return;
        }
        use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
        let Some(running) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        else {
            return;
        };
        running.unhide();
        let options = NSApplicationActivationOptions::ActivateIgnoringOtherApps
            | NSApplicationActivationOptions::ActivateAllWindows;
        if !running.activateWithOptions(options) {
            write_status_log(&format!("settings activation failed pid={pid}"));
        }
    }
}

/// `_should_open_settings_on_start`: only a deliberate foreground launch may
/// steal focus with the settings window.
pub fn should_open_settings_on_start(
    processor_available: bool,
    is_frozen: bool,
    arguments: &[String],
    restarting: bool,
    setup_complete: bool,
) -> bool {
    let _ = is_frozen;
    if arguments
        .iter()
        .any(|argument| argument == "--show-settings-after-restart")
    {
        return true;
    }
    if restarting
        || arguments
            .iter()
            .any(|argument| argument == "--background-login")
    {
        return false;
    }
    !processor_available || !setup_complete
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_policy_matches_python() {
        let args = |values: &[&str]| values.iter().map(|value| value.to_string()).collect::<Vec<_>>();
        assert!(should_open_settings_on_start(false, true, &args(&[]), false, true));
        assert!(!should_open_settings_on_start(true, true, &args(&[]), false, true));
        assert!(should_open_settings_on_start(
            true,
            true,
            &args(&["--show-settings-after-restart"]),
            false,
            true
        ));
        assert!(!should_open_settings_on_start(
            true,
            true,
            &args(&["--background-login"]),
            false,
            true
        ));
        assert!(!should_open_settings_on_start(true, true, &args(&[]), true, true));
        assert!(should_open_settings_on_start(true, true, &args(&[]), false, false));
    }

    #[test]
    fn instance_pid_gates_signaling() {
        let directory = std::env::temp_dir().join(format!(
            "veyqa-settings-launch-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&directory).unwrap();
        let instance = directory.join(READY_MARKER);
        fs::write(&instance, "1\n").unwrap();
        assert_eq!(read_pid(&instance), None);
        fs::write(&instance, format!("{}\n", std::process::id())).unwrap();
        assert_eq!(read_pid(&instance), Some(std::process::id() as libc::pid_t));
        assert!(process_alive(std::process::id() as libc::pid_t));
        assert!(!process_alive(0));
        let _ = fs::remove_dir_all(directory);
    }

    fn read_pid(path: &Path) -> Option<libc::pid_t> {
        let text = fs::read_to_string(path).ok()?;
        let pid = text.trim().parse::<libc::pid_t>().ok()?;
        (pid > 1).then_some(pid)
    }

    #[test]
    fn executable_lookup_reports_this_process() {
        let path = process_executable(std::process::id() as libc::pid_t)
            .expect("proc_pidpath should resolve the test binary");
        assert!(path.is_absolute());
        assert!(same_executable(
            std::process::id() as libc::pid_t,
            &std::env::current_exe().unwrap()
        ));
        assert!(!same_executable(std::process::id() as libc::pid_t, Path::new("/usr/bin/true")));
    }

    #[test]
    fn log_rotation_keeps_one_backup() {
        let directory = std::env::temp_dir().join(format!(
            "veyqa-settings-log-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("settings_ui.log");
        fs::write(&path, vec![b'x'; LOG_MAX_BYTES as usize]).unwrap();
        write_status_log_to(&path, "rotated");
        let rotated = directory.join("settings_ui.log.1");
        assert_eq!(fs::metadata(&rotated).unwrap().len(), LOG_MAX_BYTES);
        let fresh = fs::read_to_string(&path).unwrap();
        assert!(fresh.ends_with("rotated\n"));
        let _ = fs::remove_dir_all(directory);
    }
}
