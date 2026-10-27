# voice-input

System-wide voice input for Wayland: microphone audio is buffered for one take,
transcribed after the stop shortcut, and committed to the focused
Fcitx-compatible input context (Ghostty, Claude Code, GTK applications).

## Requirements

- Wayland session (developed against GNOME/Mutter)
- `pw-record` (PipeWire) and `notify-send` (libnotify)
- Fcitx5 5.1.0 or newer with its D-Bus addon
- CMake, pkg-config, a C++ compiler, and Fcitx5 development files for the
  user-local text-injection addon
- An ASR server exposing OpenAI-compatible `POST /v1/audio/transcriptions`,
  such as [sglang-omni](https://github.com/sgl-project/sglang-omni) serving
  Qwen3-ASR

`dotool` and `wtype` are optional legacy backends. Fcitx5 is the default because
it commits Unicode text directly instead of simulating keyboard layouts.

## Build

```bash
cargo build --release
```

## Setup

1. Start your ASR server, e.g. sgl-omni:
   ```bash
   sgl-omni serve --model-path Qwen/Qwen3-ASR-0.6B --port 8000
   ```
2. Install the Fcitx5 addon and restart Fcitx5. Build artifacts use a
   source-specific directory under `${XDG_CACHE_HOME:-$HOME/.cache}/voice-input/fcitx5`
   (override with `VOICE_INPUT_BUILD_DIR`); installation targets
   `${XDG_DATA_HOME:-$HOME/.local/share}` and never uses sudo:
   ```bash
   contrib/fcitx5-voice-input/install-user.sh
   fcitx5 -d -r
   busctl --user introspect org.fcitx.Fcitx5 /org/freedesktop/Fcitx5/VoiceInput
   ```
   The introspection output must contain `CommitText`. On NixOS, run the
   installer from a `pkgs.mkShell` containing `cmake`, `pkg-config`, `gcc`,
   `coreutils`, and the same `fcitx5` package version as the running daemon;
   rebuild after Fcitx upgrades because the addon uses its C++ ABI.
3. First run writes an annotated default config to
   `$XDG_CONFIG_HOME/voice-input/config.toml`; point `provider.url` at your
   server's `/v1/audio/transcriptions` endpoint. `inject.backend` defaults to
   `fcitx5` and requires the addon from step 2.
4. Start the daemon:
   ```bash
   voice-input daemon
   ```
5. Bind a GNOME shortcut (Settings → Keyboard → Custom Shortcuts):
   `voice-input toggle` on a key of your choice (e.g. Super+Z).

Press once to start dictation and again to stop. Nothing is typed while you are
speaking. After the second press, the complete take is transcribed and the final
text is injected once. The tool never presses Enter for you.

## Commands

- `voice-input daemon` — resident process (capture, ASR session, injection)
- `voice-input toggle` — start/stop dictation (for the desktop shortcut)
- `voice-input status` — print daemon state (`idle` / `recording` / `finalizing`)
- `voice-input probe <wav>` — transcribe a 16kHz mono wav and print the final
  result with timestamps

## Recognition

Each take is buffered in memory and sent in one OpenAI-compatible HTTP
transcription request when recording stops. There are no incremental transcript
or injection events during recording. A successful request produces one final
transcript for the whole take.

## Conservative DeepSeek polishing

Polishing is disabled by default. Enable it explicitly and provide the API key
in the daemon's environment, never in the config file:

```toml
[polish]
enabled = true
url = "https://api.deepseek.com/chat/completions"
model = "deepseek-flash"
api_key_env = "DEEPSEEK_API_KEY"
timeout_ms = 5000
```

Remote endpoints must use HTTPS. HTTP is allowed only for a loopback gateway,
for example `http://127.0.0.1:3425/v1/chat/completions`.

Only the non-empty final transcript of each take is considered for correction.
Audio, conversation history, and other takes are not sent to the polishing
service. The prompt only permits conservative ASR correction; translation,
expansion, summaries, answers, and instructions contained in the transcript are
not allowed.

A missing key produces one warning and no HTTP request. Missing keys, timeouts,
HTTP or JSON errors, empty content, control characters, and non-`stop` finish
reasons all select the original transcript. A correction that the selected
backend cannot inject (for example, Unicode punctuation through `dotool`) also
selects the original. The selected transcript gets exactly one injection
attempt; failures are never retried with alternate text because a timeout could
otherwise duplicate text. `finalizing` includes correction and injection drain,
not just the transcription request.

## Config

`$XDG_CONFIG_HOME/voice-input/config.toml` — see the annotated default written
on first run. Logs and the IPC socket follow the XDG spec
(`$XDG_STATE_HOME/voice-input/`, `$XDG_RUNTIME_DIR/voice-input.sock`).
