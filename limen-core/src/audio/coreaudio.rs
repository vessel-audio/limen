//! Microphone capture through cpal/CoreAudio (macOS).
//!
//! The device is negotiated to f32 at its preferred rate (48 kHz on built-in
//! microphones); a forwarder thread downmixes to mono and resamples to the
//! crate-wide 16 kHz s16le contract. The realtime callback itself only
//! copies blocks into vessel-capture's preallocated pool.

use std::collections::VecDeque;

use std::borrow::Cow;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use vessel_capture::{InputConfig, OverflowPolicy};

use super::chain::GRAPH_RATE;
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
        let (stream, config, handle, blocks) = open(device)?;
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

/// The device's stream, the format it settled on, and the pipeline that
/// carries its blocks out of the realtime callback.
fn open(
    device: Option<&str>,
) -> Result<(
    cpal::Stream,
    InputConfig,
    vessel_capture::CaptureHandle,
    crossbeam_channel::Receiver<Vec<f32>>,
)> {
    let device = vessel_capture::resolve_input_device(device)
        .context("failed to resolve the input device")?;
    let config = vessel_capture::negotiate_input_config(&device)
        .context("the input device has no usable f32 configuration")?;
    let (handle, blocks) =
        vessel_capture::block_pipeline(&config, QUEUE_BLOCKS, OverflowPolicy::DropNewest)
            .context("failed to set up the capture pipeline")?;
    let stream = vessel_capture::start_input_stream(&device, config, &handle)
        .context("failed to start the input stream")?;
    Ok((stream, config, handle, blocks))
}

/// One block as mono: itself when it already is, else the average of each
/// left/right pair.
fn mono(block: &[f32], channels: usize) -> Cow<'_, [f32]> {
    match channels {
        1 => Cow::Borrowed(block),
        _ => Cow::Owned(
            block
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[left, right]| (left + right) / 2.0)
                .collect(),
        ),
    }
}

/// Microphone capture for the plugin graph: mono f32 at the graph's rate,
/// unquantized, so plugins see what the device delivered.
pub struct FloatCapture {
    // Dropping the stream stops the AudioUnit.
    _stream: cpal::Stream,
    samples: mpsc::Receiver<Vec<f32>>,
    pending: VecDeque<f32>,
}

impl FloatCapture {
    /// Start recording from the named CoreAudio device (`None` = default).
    pub fn start(device: Option<&str>) -> Result<Self> {
        let (stream, config, handle, blocks) = open(device)?;
        let (tx, samples) = mpsc::channel(PCM_CHUNKS);
        std::thread::Builder::new()
            .name("limen-capture".into())
            .spawn(move || forward_float(config, handle, blocks, tx))
            .context("failed to spawn the capture forwarder")?;
        Ok(Self {
            _stream: stream,
            samples,
            pending: VecDeque::new(),
        })
    }

    /// Read exactly `samples` samples; `Ok(None)` at end of stream. Safe to
    /// drop mid-await: what was already received is kept.
    pub async fn read_chunk(&mut self, samples: usize) -> Result<Option<Vec<f32>>> {
        loop {
            if self.pending.len() >= samples {
                return Ok(Some(self.pending.drain(..samples).collect()));
            }
            match self.samples.recv().await {
                Some(chunk) => self.pending.extend(chunk),
                None if self.pending.is_empty() => return Ok(None),
                None => return Ok(Some(self.pending.drain(..).collect())),
            }
        }
    }
}

/// Downmix and resample blocks to the graph's rate until the stream or the
/// consumer goes away.
fn forward_float(
    config: InputConfig,
    handle: vessel_capture::CaptureHandle,
    blocks: crossbeam_channel::Receiver<Vec<f32>>,
    samples: mpsc::Sender<Vec<f32>>,
) {
    let channels = usize::from(config.channels);
    let mut resampler = plugin_graph::Resampler::new(config.sample_rate as u32, GRAPH_RATE);
    while let Ok(block) = blocks.recv() {
        let mut out = Vec::new();
        resampler.push(&mono(&block, channels), &mut out);
        handle.recycle(block);
        match samples.try_send(out) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("sample channel full, dropping a chunk")
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return,
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
        resampler.push(&mono(&block, channels), &mut out);
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
