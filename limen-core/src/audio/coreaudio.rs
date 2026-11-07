//! Microphone capture through cpal/CoreAudio (macOS).
//!
//! The device is negotiated to f32 at its preferred rate (48 kHz on built-in
//! microphones); a forwarder thread downmixes to mono and resamples to the
//! crate-wide 16 kHz s16le contract. The realtime callback itself only
//! copies blocks into vessel-capture's preallocated pool.

use std::collections::VecDeque;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use vessel_capture::{InputConfig, OverflowPolicy};

use super::resample::LinearResampler;

/// Blocks of 256 frames in flight between the callback and the forwarder;
/// at 48 kHz one block is ~5 ms, so this is ~340 ms of slack.
const QUEUE_BLOCKS: usize = 64;
/// Resampled chunks buffered for the ASR session.
const PCM_CHUNKS: usize = 64;

pub struct Capture {
    // Dropping the stream stops the AudioUnit.
    _stream: cpal::Stream,
    pcm: mpsc::Receiver<Vec<i16>>,
    pending: VecDeque<i16>,
}

impl Capture {
    /// Start recording from the named CoreAudio device (`None` = default).
    /// `device` is an exact `cpal` device name.
    pub fn start(device: Option<&str>) -> Result<Self> {
        let device = vessel_capture::resolve_input_device(device)
            .context("failed to resolve the input device")?;
        let config = vessel_capture::negotiate_input_config(&device)
            .context("the input device has no usable f32 configuration")?;
        let (handle, blocks) =
            vessel_capture::block_pipeline(&config, QUEUE_BLOCKS, OverflowPolicy::DropNewest)
                .context("failed to set up the capture pipeline")?;
        let stream = vessel_capture::start_input_stream(&device, config, &handle)
            .context("failed to start the input stream")?;

        let (pcm_tx, pcm) = mpsc::channel(PCM_CHUNKS);
        std::thread::Builder::new()
            .name("limen-capture".into())
            .spawn(move || forward_blocks(config, handle, blocks, pcm_tx))
            .context("failed to spawn the capture forwarder")?;

        Ok(Self {
            _stream: stream,
            pcm,
            pending: VecDeque::new(),
        })
    }

    /// Read exactly `samples` s16le samples; `Ok(None)` at end of stream.
    pub async fn read_chunk(&mut self, samples: usize) -> Result<Option<Vec<i16>>> {
        loop {
            if self.pending.len() >= samples {
                return Ok(Some(self.pending.drain(..samples).collect()));
            }
            match self.pcm.recv().await {
                Some(chunk) => self.pending.extend(chunk),
                // Closed and drained: report whatever is left, then EOF.
                None if self.pending.is_empty() => return Ok(None),
                None => return Ok(Some(self.pending.drain(..).collect())),
            }
        }
    }
}

/// Downmix and resample blocks until the stream or the consumer goes away.
fn forward_blocks(
    config: InputConfig,
    handle: vessel_capture::CaptureHandle,
    blocks: crossbeam_channel::Receiver<Vec<f32>>,
    pcm: mpsc::Sender<Vec<i16>>,
) {
    let channels = usize::from(config.channels);
    let mut resampler = LinearResampler::new(config.sample_rate as u32);
    while let Ok(block) = blocks.recv() {
        let mut out = Vec::with_capacity(block.len() / channels / 2);
        match channels {
            1 => resampler.push(&block, &mut out),
            _ => {
                let mono: Vec<f32> = block
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|[left, right]| (left + right) / 2.0)
                    .collect();
                resampler.push(&mono, &mut out);
            }
        }
        handle.recycle(block);
        // A lagging ASR session must not stall the microphone: drop the
        // chunk rather than blocking, mirroring forward_chunks' policy.
        match pcm.try_send(out) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("pcm channel full, dropping a chunk")
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return,
        }
    }
}
