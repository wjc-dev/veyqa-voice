//! Keep one microphone and hotkey owner per runtime data directory.

use std::os::fd::RawFd;
use std::path::Path;

use anyhow::{Context, Result};

pub struct AppInstance {
    descriptor: RawFd,
}

impl AppInstance {
    /// Returns `Ok(None)` when another process already owns the directory.
    pub fn acquire(directory: &Path) -> Result<Option<Self>> {
        let path = directory.join(".app-instance.lock");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("无法创建 {}", parent.display()))?;
        }
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .context("实例锁路径包含 NUL")?;
        let descriptor = unsafe {
            libc::open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("无法打开 {}", path.display()));
        }
        let locked = unsafe { libc::flock(descriptor, libc::LOCK_EX | libc::LOCK_NB) };
        if locked == 0 {
            return Ok(Some(Self { descriptor }));
        }
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(descriptor) };
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        Err(error).with_context(|| format!("无法锁定 {}", path.display()))
    }
}

impl Drop for AppInstance {
    fn drop(&mut self) {
        // Closing also releases the flock after normal exit or panic.
        unsafe { libc::close(self.descriptor) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn only_one_owner_per_directory() {
        let directory = std::env::temp_dir().join(format!(
            "veyqa-instance-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&directory).unwrap();

        let first = AppInstance::acquire(&directory).unwrap();
        assert!(first.is_some());
        assert!(AppInstance::acquire(&directory).unwrap().is_none());

        let lock = directory.join(".app-instance.lock");
        let mode = std::fs::metadata(&lock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        drop(first);
        assert!(AppInstance::acquire(&directory).unwrap().is_some());
        let _ = std::fs::remove_dir_all(directory);
    }
}
