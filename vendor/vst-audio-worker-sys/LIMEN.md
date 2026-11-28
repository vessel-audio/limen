# vst-audio-worker-sys, patched for limen

A copy of `vst-audio-worker-sys` 0.13.0 (upstream: https://github.com/115dkk/vst-audio,
under the licences in this directory) with one change, selected by
`[patch.crates-io]` in the workspace `Cargo.toml` so that plain cargo and the
nix build both use it. The copy is not a workspace member (`exclude = ["vendor"]`),
so workspace tests, clippy and rustfmt leave it alone.

**The change** (`src/vst3.rs`, `setProcessing(1)` on the audio path): a plugin
answering `kNotImplemented` is no longer a fault. The VST3 spec lets a plugin
leave `setProcessing` unimplemented, and hosts carry on. Upstream treats any
answer other than `kResultOk` as a failure and stops the worker on the first
audio block, so Kilohearts' Carve EQ could never process: its node was always
passed through.

Upstream's `main` (checked 2026-11-28, release 0.13.0) has the same check, and
no issue about it. Drop this directory and the `[patch.crates-io]` entry once a
release has the fix, or when the VST3 host is replaced.

```diff
diff --git a/src/vst3.rs b/src/vst3.rs
index 97e8d95..a12c086 100644
--- a/src/vst3.rs
+++ b/src/vst3.rs
@@ -2005,7 +2005,10 @@ impl Vst3Processor {
             if !self.processing {
                 // SAFETY: sole audio handle, active configured component, entering processing.
                 let status = unsafe { self.shared.processor.setProcessing(1) };
-                if status != kResultOk {
+                // The VST3 spec lets a plugin leave setProcessing unimplemented, and
+                // hosts carry on without it (Kilohearts' Carve EQ answers
+                // kNotImplemented). Only a real refusal is a fault.
+                if status != kResultOk && status != kNotImplemented {
                     plugin_failed = Some(AudioFailure::SetProcessing(status));
                     return false;
                 }
```
