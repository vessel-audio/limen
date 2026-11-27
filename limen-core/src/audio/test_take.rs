//! A test recording: the microphone for a few seconds, kept before and after
//! the routing graph so the user can hear what the plugins do to their
//! voice.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};

use super::FloatCapture;
use super::chain::{GRAPH_RATE, to_speech};
use super::plugins::PluginChains;
use crate::ipc::audio::TestFiles;

/// The longest test, in seconds.
pub(crate) const MAX_TEST_SECONDS: u32 = 30;

impl PluginChains {
    fn test_dir(&self) -> PathBuf {
        self.paths.cache_dir.join("audio-test")
    }

    /// Stop the test recording in progress, if there is one; a take that is
    /// starting needs the microphone.
    pub(crate) fn cancel_test(&self) {
        let running = self.test.lock().ok().and_then(|slot| slot.clone());
        if let Some(cancel) = running {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Record `seconds` of the microphone, then process the same recording
    /// through the saved graph. Both come out as 16 kHz mono WAV files.
    pub(crate) async fn record_test(
        self: &Arc<Self>,
        seconds: u32,
        device: Option<String>,
    ) -> Result<TestFiles> {
        if !(1..=MAX_TEST_SECONDS).contains(&seconds) {
            bail!("a test lasts 1 to {MAX_TEST_SECONDS} seconds");
        }
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut slot = self
                .test
                .lock()
                .map_err(|_| anyhow::anyhow!("test state poisoned"))?;
            if slot.is_some() {
                bail!("a test is already recording");
            }
            *slot = Some(cancel.clone());
        }
        let recorded = self.record(seconds, device, &cancel).await;
        if let Ok(mut slot) = self.test.lock() {
            *slot = None;
        }
        let samples = recorded?;
        let chains = self.clone();
        tokio::task::spawn_blocking(move || chains.process_test(&samples))
            .await
            .context("the test processing stopped")?
    }

    async fn record(
        &self,
        seconds: u32,
        device: Option<String>,
        cancel: &AtomicBool,
    ) -> Result<Vec<f32>> {
        let wanted = (GRAPH_RATE * seconds) as usize;
        let mut capture = FloatCapture::start(device.as_deref())?;
        let mut samples = Vec::with_capacity(wanted);
        while samples.len() < wanted {
            if cancel.load(Ordering::Relaxed) {
                bail!("the test was cancelled because a dictation started");
            }
            match capture.read_chunk(GRAPH_RATE as usize / 10).await? {
                Some(chunk) => samples.extend(chunk),
                None => bail!("microphone capture ended"),
            }
        }
        Ok(samples)
    }

    fn process_test(&self, samples: &[f32]) -> Result<TestFiles> {
        let dry = to_speech(samples);
        let wet = match self.build() {
            Some(mut chain) => {
                let mut wet = samples
                    .chunks(GRAPH_RATE as usize / 10)
                    .flat_map(|chunk| chain.feed(chunk))
                    .collect::<Vec<_>>();
                wet.extend(chain.finish());
                wet
            }
            None => dry.clone(),
        };
        let dir = self.test_dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
        let write = |name: &str, pcm: &[i16]| -> Result<String> {
            let path = dir.join(name);
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: super::SAMPLE_RATE,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::create(&path, spec)
                .with_context(|| format!("failed to write {}", path.display()))?;
            pcm.iter()
                .try_for_each(|sample| writer.write_sample(*sample))?;
            writer.finalize()?;
            Ok(path.display().to_string())
        };
        Ok(TestFiles {
            dry: write("dry.wav", &dry)?,
            wet: write("wet.wav", &wet)?,
        })
    }
}

impl Drop for PluginChains {
    /// The recordings are of the user's voice: they do not outlive the daemon.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.test_dir());
    }
}
