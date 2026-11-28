//! Microphone capture via the system `pw-record` (PipeWire) binary.
//!
//! Spawns `pw-record --container raw --format s16 --rate 16000 --channels 1 -`
//! and reads raw s16le PCM straight off its stdout. (`--container raw` matters:
//! writing to `-` makes pw-record pick a container we can't parse, while `raw`
//! is header-less.)

use std::process::Stdio;
use std::time::Duration;

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
        Self::spawn(device, "s16", "16000")
    }

    fn spawn(device: Option<&str>, format: &str, rate: &str) -> Result<Self> {
        let mut cmd = Command::new("pw-record");
        cmd.args([
            "--container",
            "raw",
            "--format",
            format,
            "--rate",
            rate,
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

/// Microphone capture for the plugin graph: mono f32 at the graph's rate,
/// which PipeWire converts to, so plugins see unquantized audio.
pub struct FloatCapture {
    capture: Capture,
    /// Bytes read and not yet a whole chunk.
    partial: Vec<u8>,
}

impl FloatCapture {
    /// Start recording from the given pw-record target (`None` = default).
    pub fn start(device: Option<&str>) -> Result<Self> {
        Ok(Self {
            capture: Capture::spawn(device, "f32", "48000")?,
            partial: Vec::new(),
        })
    }

    fn samples(bytes: &[u8]) -> Vec<f32> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect()
    }

    /// Stop the microphone and collect everything read and not yet
    /// returned, down to the last whole sample: the end of a take that is
    /// stopping. What pw-record had already written to the pipe is read out
    /// before it ends.
    pub async fn drain(&mut self) -> Vec<f32> {
        let _ = self.capture.child.start_kill();
        if let Some(stdout) = self.capture.child.stdout.as_mut() {
            let partial = &mut self.partial;
            let _ = tokio::time::timeout(Duration::from_millis(300), async {
                let mut buf = [0u8; 8192];
                loop {
                    match stdout.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => partial.extend_from_slice(&buf[..read]),
                    }
                }
            })
            .await;
        }
        let samples = Self::samples(&self.partial);
        self.partial.clear();
        samples
    }

    /// Read exactly `samples` samples; `Ok(None)` on EOF. Safe to drop
    /// mid-await: what was already read is kept for the next call.
    pub async fn read_chunk(&mut self, samples: usize) -> Result<Option<Vec<f32>>> {
        let wanted = samples * 4;
        let stdout = self
            .capture
            .child
            .stdout
            .as_mut()
            .context("pw-record stdout")?;
        while self.partial.len() < wanted {
            let mut buf = [0u8; 8192];
            match stdout.read(&mut buf).await? {
                0 => {
                    let rest = Self::samples(&self.partial);
                    self.partial.clear();
                    return Ok(match rest.is_empty() {
                        true => None,
                        false => Some(rest),
                    });
                }
                read => self.partial.extend_from_slice(&buf[..read]),
            }
        }
        let chunk = Self::samples(&self.partial[..wanted]);
        self.partial.drain(..wanted);
        Ok(Some(chunk))
    }
}
