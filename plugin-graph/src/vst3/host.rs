//! Starting plugin workers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use vst_audio::{BusSelector, Configuration, Host, PluginDescriptor, PluginFormat, StartError};

use super::buses;
use super::control::Vst3Control;
use super::processor::Vst3Processor;
use crate::Sidechain;

/// Which channels to ask a plugin's main buses for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channels {
    /// One channel, so a mono microphone needs no stereo wiring; when the
    /// plugin refuses, the arrangement it declares.
    PreferMono,
    /// The arrangement the plugin declares.
    AsDeclared,
}

/// How a plugin is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadOptions {
    pub sample_rate: u32,
    /// Frames per block: the graph's block, which every `process` call must
    /// match.
    pub block: usize,
    pub channels: Channels,
    pub sidechain: Sidechain,
    /// How long a block may take before the plugin counts as failed.
    pub deadline: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum Vst3Error {
    #[error("{0}")]
    Start(#[from] StartError),
    #[error("the plugin has no audio endpoint")]
    NoAudio,
    #[error("no answer from the plugin within {0:?}")]
    Timeout(Duration),
    #[error("{0}")]
    Plugin(String),
    #[error("the block size {0} does not fit a plugin")]
    Block(usize),
    #[error("{0:?} is not a class id: 32 hexadecimal digits are expected")]
    ClassId(String),
}

/// A class id as written by a scan: 16 bytes of lowercase hexadecimal, or
/// nothing for the bundle's first class.
fn parse_class_id(text: &str) -> Result<Option<[u8; 16]>, Vst3Error> {
    match text.is_empty() {
        true => Ok(None),
        false => {
            let bad = || Vst3Error::ClassId(text.to_string());
            let bytes = text
                .as_bytes()
                .chunks(2)
                .map(|pair| {
                    std::str::from_utf8(pair)
                        .ok()
                        .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                })
                .collect::<Option<Vec<u8>>>()
                .ok_or_else(bad)?;
            bytes.try_into().map(Some).map_err(|_| bad())
        }
    }
}

/// Starts plugin workers.
pub struct Vst3Host {
    host: Host,
}

impl Vst3Host {
    /// A host whose workers are the executable at `worker`
    /// (`vst-audio-worker`).
    pub fn new(worker: impl Into<PathBuf>) -> Self {
        Self {
            host: Host::new(worker),
        }
    }

    pub(super) fn inner(&self) -> &Host {
        &self.host
    }

    /// Start the plugin class `class_id` (lowercase hexadecimal, as the scan
    /// reports it; empty for the first class) of the bundle at `bundle` in a
    /// worker, restoring `state` when given. The control handle owns the
    /// worker: dropping it stops the plugin, so keep it for as long as the
    /// processor is in use.
    pub fn load(
        &self,
        bundle: &Path,
        class_id: &str,
        options: LoadOptions,
        state: Option<&[u8]>,
    ) -> Result<(Vst3Control, Vst3Processor), Vst3Error> {
        let class = parse_class_id(class_id)?;
        let quantum = u32::try_from(options.block).map_err(|_| Vst3Error::Block(options.block))?;
        // The selector runs while the worker starts, and says how many
        // channels the main input bus got.
        let main_inputs = Arc::new(AtomicU16::new(0));
        let start = |channels| {
            let main_inputs = main_inputs.clone();
            let configuration = Configuration {
                sample_rate: options.sample_rate,
                quantum,
                buses: Some(BusSelector::new(move |inputs, outputs| {
                    let choice = buses::choose(inputs, outputs, channels, options.sidechain)?;
                    main_inputs.store(
                        choice
                            .inputs
                            .iter()
                            .find(|bus| bus.active)
                            .map_or(0, |bus| bus.layout.channels),
                        Ordering::Relaxed,
                    );
                    Ok(choice)
                })),
                ..Configuration::default()
            };
            let mut descriptor = PluginDescriptor::new(bundle, PluginFormat::Vst3);
            descriptor.class_id = class;
            self.host.spawn(&descriptor, &configuration, state)
        };
        let mut instance = match options.channels {
            Channels::PreferMono => {
                start(Channels::PreferMono).or_else(|_| start(Channels::AsDeclared))
            }
            Channels::AsDeclared => start(Channels::AsDeclared),
        }?;
        let blocks = instance.take_blocks().ok_or(Vst3Error::NoAudio)?;
        Ok((
            Vst3Control::new(instance),
            Vst3Processor::new(
                blocks,
                usize::from(main_inputs.load(Ordering::Relaxed)),
                options,
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_class_id;

    #[test]
    fn a_class_id_is_sixteen_bytes_of_hex_or_nothing() {
        let id = "000102030405060708090a0b0c0d0e0f";
        assert_eq!(
            (
                parse_class_id("").unwrap(),
                parse_class_id(id).unwrap(),
                parse_class_id("00ff").is_err(),
                parse_class_id(&id.replace('0', "z")).is_err(),
            ),
            (
                None,
                Some([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]),
                true,
                true
            )
        );
    }
}
