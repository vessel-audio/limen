//! The worker that `vst-audio` starts for each plugin. It is the published
//! `vst-audio-worker` executable, built here from the same runtime crate so
//! that it goes through the workspace's lockfile, and so lands in the same
//! place as `limen` in every package, instead of needing a fetch of its own.

fn main() {
    vst_audio_worker_runtime::host::main();
}
