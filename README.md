# Limen

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

The GUI has desktop end-to-end tests. Inside `nix develop`, `cargo gui-e2e`
launches the control panel in a private Xvfb display with its own D-Bus and
AT-SPI buses, finds every control through the accessibility tree, presses it
with real pointer and keyboard input, and checks what the desktop sees: the
clipboard, window-manager requests, rendered colors, programs started, GNOME
settings, the config file, and the daemon protocol. Pass part of a case name to
run fewer cases, such as `cargo gui-e2e shortcuts`. Each case leaves a
screenshot, its accessibility tree, and logs under `target/tmp/gui-e2e/`.

## GUI control panel

Open the GPUI control panel with `limen gui` or the desktop launcher. The
default Nix package includes it; `nix run .#headless -- daemon` builds and runs
the headless variant without GPUI. Closing the window never stops the daemon,
so shortcut dictation keeps working. If the daemon is not running, the GUI starts
it in the background and keeps its log at the XDG state path.

The dictation, history, and shortcut pages update live from the daemon. Stopping
a take with the GUI button keeps the polished text in history and copies it to
the clipboard instead of typing it into the GUI. The level meter and recent
errors are visible on the dictation page. Use the about page to open the config
or log, restart the background service, or stop it after confirmation. If an
upgrade leaves an outdated daemon running, restart it from the GUI so the new
protocol takes effect.

The Models pages, opened from the sidebar or the Models button on the dictation
page, set up the ASR provider (server and route, API key, model) and the
polishing provider (on or off, URL, API key, model). The model list shows what
the server offers through `GET …/models` and also takes a typed name. Saving
writes `config.toml` and applies the change to a running daemon from its next
take. API keys never go into `config.toml`: they are saved in `keys.toml` beside
it with mode 0600, and a saved key comes before the variable named by
`api_key_env`.

## Setup

1. Start your ASR server, e.g. sgl-omni:
   ```bash
   sgl-omni serve --model-path Qwen/Qwen3-ASR-0.6B --port 8000
   ```
2. Install the Fcitx5 addon and restart Fcitx5. Build artifacts use a
   source-specific directory under `${XDG_CACHE_HOME:-$HOME/.cache}/limen/fcitx5`
   (override with `LIMEN_BUILD_DIR`); installation targets
   `${XDG_DATA_HOME:-$HOME/.local/share}` and never uses sudo:
   ```bash
   contrib/fcitx5-limen/install-user.sh
   fcitx5 -d -r
   busctl --user introspect org.fcitx.Fcitx5 /org/freedesktop/Fcitx5/Limen
   ```
   The introspection output must contain `CommitText`. On NixOS, `nix develop`
   provides CMake, pkg-config, GCC, coreutils, and Fcitx5 development files.
   Use the same `fcitx5` package version as the running daemon; rebuild after
   Fcitx upgrades because the addon uses its C++ ABI.
3. First run writes an annotated default config to
   `$XDG_CONFIG_HOME/limen/config.toml`; point `provider.url` at your
   server's `/v1/audio/transcriptions` endpoint. `input_method.backend` defaults to
   `fcitx5` and requires the addon from step 2.
4. Start the daemon:
   ```bash
   limen daemon
   ```
   The portal shortcuts below live in the daemon, so keep it running, e.g.
   by starting it from your session's autostart; Limen does not
   install an autostart entry yet.
5. Bind the shortcuts, choosing one way:
   - **Portal (recommended, supports hold-to-talk):**
     ```bash
     limen shortcut setup portal
     ```
     The first time, GNOME asks you to confirm the keys, prefilled with
     Super+Z (toggle) and Super+X (hold to talk); pass `--toggle KEYS` or
     `--hold KEYS` to suggest others. GNOME keeps the binding from then on:
     change it in GNOME Settings with `limen shortcut edit`. Without a
     Nix package, setup installs the desktop entry the portal needs into
     `~/.local/share/applications`.
   - **GNOME custom shortcut (toggle only):**
     ```bash
     limen shortcut setup gnome-keybinding --toggle Super+Z
     ```
     It writes a GNOME custom shortcut that runs `limen toggle`, keeping
     your other custom shortcuts, and refuses a key GNOME already uses. Change
     the key later with `limen shortcut set toggle KEYS`.
   - **By hand:** bind `limen toggle` in your desktop's keyboard
     settings (backend `none`, the default). Compositors with key-release
     bindings can also run `limen hold press` and `limen hold
     release`, e.g. sway's `bindsym --no-repeat` and `bindsym --release`.

   `limen shortcut status` shows what is bound. A config managed by
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

- `limen gui` — open the graphical control panel (GUI-enabled builds)
- `limen daemon` — resident process (capture, ASR session, text input,
  portal shortcuts)
- `limen toggle` — start/stop dictation (for the desktop shortcut)
- `limen hold press|release` — the hold-to-talk key went down or up (for
  key-release bindings)
- `limen status` — print daemon state (`idle` / `recording` / `finalizing`)
- `limen shortcut status|setup|set|clear|edit` — show, set up, or change
  the global shortcuts; exits with 1 on failure and 2 on invalid arguments
- `limen probe <wav>` — transcribe a 16kHz mono wav and print the final
  result with timestamps

## Recognition

Each take is buffered in memory and sent in one OpenAI-compatible HTTP
transcription request when recording stops. There are no incremental transcript
or text input events during recording. A successful request produces one final
transcript for the whole take.

## Conservative DeepSeek polishing

Polishing is disabled by default. Enable it on the GUI's Polish page, or in the
config with the API key in the daemon's environment, never in the config file:

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

`$XDG_CONFIG_HOME/limen/config.toml` — see the annotated default written
on first run. Logs and the IPC socket follow the XDG spec
(`$XDG_STATE_HOME/limen/`, `$XDG_RUNTIME_DIR/limen.sock`).
