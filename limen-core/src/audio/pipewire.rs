//! Microphone capture via the system `pw-record` (PipeWire) binary.
//!
//! Spawns `pw-record --container raw --format s16 --rate 16000 --channels 1 -`
//! and reads raw s16le PCM straight off its stdout. (`--container raw` matters:
//! writing to `-` makes pw-record pick a container we can't parse, while `raw`
//! is header-less.)

use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use super::CAPTURE_REAP_TIMEOUT;

pub struct Capture {
    child: Child,
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let deadline = std::time::Instant::now() + CAPTURE_REAP_TIMEOUT;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(None) => {
                    tracing::warn!("pw-record did not exit within the reap timeout");
                    return;
                }
            }
        }
    }
}

impl Capture {
    /// Start recording from the given pw-record target (`None` = default).
    pub fn start(device: Option<&str>) -> Result<Self> {
        let mut cmd = Command::new("pw-record");
        cmd.args([
            "--container",
            "raw",
            "--format",
            "s16",
            "--rate",
            "16000",
            "--channels",
            "1",
        ])
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Reaping is driven by dropping the capture task.
        .kill_on_drop(true);
        if let Some(dev) = device {
            cmd.args(["--target", dev]);
        }
        let child = cmd.spawn().context("failed to spawn pw-record")?;
        Ok(Self { child })
    }

    /// Read exactly `samples` s16le samples; `Ok(None)` on EOF.
    pub async fn read_chunk(&mut self, samples: usize) -> Result<Option<Vec<i16>>> {
        let stdout = self.child.stdout.as_mut().context("pw-record stdout")?;
        let mut buf = vec![0u8; samples * 2];
        match stdout.read_exact(&mut buf).await {
            Ok(_) => Ok(Some(
                buf.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|bytes| i16::from_le_bytes(*bytes))
                    .collect(),
            )),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
