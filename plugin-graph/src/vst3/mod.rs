//! Hosting VST3 plugins.
//!
//! Each plugin runs in its own worker process (`vst-audio-worker`), so a
//! plugin that crashes or hangs cannot take the application down. Everything
//! here is synchronous and uses no async runtime: the caller picks the
//! threads. Audio crosses to the worker over shared memory, which is bounded
//! and fast (a block takes well under a millisecond) but not hard-realtime,
//! so run [`Vst3Processor::process`] on a thread that may wait a few
//! milliseconds, not on a device callback.

mod buses;
mod control;
mod host;
mod processor;
mod scan;

pub use control::{PluginEvent, Vst3Control};
pub use host::{Channels, LoadOptions, Vst3Error, Vst3Host};
pub use processor::Vst3Processor;
pub use scan::{PluginInfo, Sandbox, Scan, default_folders, find_bundles};
