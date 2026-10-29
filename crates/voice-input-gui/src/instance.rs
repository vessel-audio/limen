//! Single-instance ownership for the GPUI process.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;

use anyhow::{Context, Result};
use voice_input_core::Paths;

/// Hold the process-wide GUI lock for as long as the window is open.
pub fn acquire(paths: &Paths) -> Result<Option<File>> {
    let directory = paths.socket.parent().context("socket path has no parent")?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    let path = directory.join("voice-input-gui.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    // SAFETY: flock operates on this process's valid file descriptor, with no
    // pointers or borrowed data. Failure is handled through errno below.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(Some(file));
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK => {
            println!("voice-input GUI is already open");
            Ok(None)
        }
        _ => Err(error).context("failed to lock the GUI instance"),
    }
}

#[cfg(test)]
mod tests {
    use super::acquire;
    use voice_input_core::Paths;

    #[test]
    fn a_second_instance_is_rejected_until_the_first_lock_is_released() {
        let root =
            std::env::temp_dir().join(format!("voice-input-gui-instance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let paths = Paths {
            config_file: root.join("config.toml"),
            socket: root.join("voice-input.sock"),
            log_file: root.join("daemon.log"),
            cache_dir: root.join("cache"),
        };
        let first = acquire(&paths).unwrap();
        assert!(first.is_some());
        assert!(acquire(&paths).unwrap().is_none());
        drop(first);
        assert!(acquire(&paths).unwrap().is_some());
        let _ = std::fs::remove_dir_all(root);
    }
}
