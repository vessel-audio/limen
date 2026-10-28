# voice-input

System-wide voice input for Wayland: microphone audio is buffered for one take,
transcribed after the stop shortcut, and committed to the focused
Fcitx-compatible input context (Ghostty, Claude Code, GTK applications).

## Requirements

- Wayland session (developed against GNOME/Mutter)
- `pw-record` (PipeWire) and `notify-send` (libnotify)
- Fcitx5 5.1.0 or newer with its D-Bus addon
- CMake, pkg-config, a C++ compiler, and Fcitx5 development files for the
  user-local text-input addon
- An ASR server exposing OpenAI-compatible `POST /v1/audio/transcriptions`,
  such as [sglang-omni](https://github.com/sgl-project/sglang-omni) serving
  Qwen3-ASR
- For hold-to-talk, a desktop with the GlobalShortcuts portal (GNOME 48 or
  newer); toggling works with any desktop shortcut

`dotool` and `wtype` are optional legacy backends. Fcitx5 is the default because
it commits Unicode text directly instead of simulating keyboard layouts.

## Build

### Cargo

```bash
cargo build --release
```

### Nix / flake-parts

Build the default package and run the daemon from the flake:

```bash
nix build
nix run . -- daemon
```

For development, enter the Rust and Fcitx addon build environment, run all
flake checks, or format the project:

```bash
nix develop
nix flake check
nix fmt
```

The package provides `pw-record` and `notify-send` in its runtime PATH and
installs the desktop entry that the shortcut portal needs. It does not install
the Fcitx addon; follow the separate addon setup below.

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
   The introspection output must contain `CommitText`. On NixOS, `nix develop`
   provides CMake, pkg-config, GCC, coreutils, and Fcitx5 development files.
   Use the same `fcitx5` package version as the running daemon; rebuild after
   Fcitx upgrades because the addon uses its C++ ABI.
3. First run writes an annotated default config to
   `$XDG_CONFIG_HOME/voice-input/config.toml`; point `provider.url` at your
   server's `/v1/audio/transcriptions` endpoint. `input_method.backend` defaults to
   `fcitx5` and requires the addon from step 2.
4. Start the daemon:
   ```bash
   voice-input daemon
   ```
   The portal shortcuts below live in the daemon, so keep it running, e.g.
   by starting it from your session's autostart; voice-input does not
   install an autostart entry yet.
5. Bind the shortcuts, choosing one way:
   - **Portal (recommended, supports hold-to-talk):**
     ```bash
     voice-input shortcut setup portal
     ```
     The first time, GNOME asks you to confirm the keys, prefilled with
     Super+Z (toggle) and Super+X (hold to talk); pass `--toggle KEYS` or
     `--hold KEYS` to suggest others. GNOME keeps the binding from then on:
     change it in GNOME Settings with `voice-input shortcut edit`. Without a
     Nix package, setup installs the desktop entry the portal needs into
     `~/.local/share/applications`.
   - **GNOME custom shortcut (toggle only):**
     ```bash
     voice-input shortcut setup gnome-keybinding --toggle Super+Z
     ```
     It writes a GNOME custom shortcut that runs `voice-input toggle`, keeping
     your other custom shortcuts, and refuses a key GNOME already uses. Change
     the key later with `voice-input shortcut set toggle KEYS`.
   - **By hand:** bind `voice-input toggle` in your desktop's keyboard
     settings (backend `none`, the default). Compositors with key-release
     bindings can also run `voice-input hold press` and `voice-input hold
     release`, e.g. sway's `bindsym --no-repeat` and `bindsym --release`.

   `voice-input shortcut status` shows what is bound. A config managed by
   home-manager is never rewritten: setup prints the `[shortcut]` section to
   add to it instead.

Press the toggle once to start dictation and again to stop. Nothing is typed
while you are speaking. After the second press, the complete take is
transcribed and the final text is committed once. The tool never presses
Enter for you.

With hold-to-talk, hold the key while you speak; releasing it stops and
transcribes. A hold shorter than 300ms counts as an accidental tap and is
dropped, and a hold longer than 60s is stopped and transcribed as if you had
released the key.

## Commands

- `voice-input daemon` — resident process (capture, ASR session, text input,
  portal shortcuts)
- `voice-input toggle` — start/stop dictation (for the desktop shortcut)
- `voice-input hold press|release` — the hold-to-talk key went down or up (for
  key-release bindings)
- `voice-input status` — print daemon state (`idle` / `recording` / `finalizing`)
- `voice-input shortcut status|setup|set|clear|edit` — show, set up, or change
  the global shortcuts; exits with 1 on failure and 2 on invalid arguments
- `voice-input probe <wav>` — transcribe a 16kHz mono wav and print the final
  result with timestamps

## Recognition

Each take is buffered in memory and sent in one OpenAI-compatible HTTP
transcription request when recording stops. There are no incremental transcript
or text input events during recording. A successful request produces one final
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
input method cannot commit (for example, Unicode punctuation through `dotool`) also
selects the original. The selected transcript gets exactly one commit
attempt; failures are never retried with alternate text because a timeout could
otherwise duplicate text. `finalizing` includes correction and text input drain,
not just the transcription request.

## Config

`$XDG_CONFIG_HOME/voice-input/config.toml` — see the annotated default written
on first run. Logs and the IPC socket follow the XDG spec
(`$XDG_STATE_HOME/voice-input/`, `$XDG_RUNTIME_DIR/voice-input.sock`).
