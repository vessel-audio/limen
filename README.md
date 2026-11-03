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
- An ASR service, one of:
  - a server exposing OpenAI-compatible `POST /v1/audio/transcriptions`, such
    as [sglang-omni](https://github.com/sgl-project/sglang-omni) serving
    Qwen3-ASR
  - the whisper.cpp server, which takes the take at exactly its configured URL
  - an API key for OpenAI, Groq, Mistral, or Gemini
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
page, show each role's providers as cards, the saved one marked; the Polish page
also has the switch that turns polishing on or off. A card opens a dialog: a
vendor's needs only an API key and a model, and Custom names your own server by
its URL and the API it speaks, OpenAI-compatible, whisper.cpp for ASR, or any
vendor's. No server or model is set until you choose one. Once the server
address or key stops changing, the dialog asks the provider for its models: a
server that offers exactly one fills it in, and otherwise the list offers them
and also takes a typed name; Detect asks at once. The whisper.cpp server lists
none. Test asks the model to do its job once with the fields as typed, without
saving them. Saving writes `config.toml` and applies the change to a running
daemon from its next take. API keys never go into
`config.toml`: they are saved in `keys.toml` beside it with mode 0600, and a
saved key comes before the variable named by `api_key_env`, which defaults to
the vendor's own, such as `OPENAI_API_KEY`.

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
   `$XDG_CONFIG_HOME/limen/config.toml` with no ASR server or model; set them
   on the GUI's ASR page, or point `provider.url` at your server's
   `/v1/audio/transcriptions` endpoint and set `provider.model`. `input_method.backend` defaults to
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

Each take is buffered in memory and sent in one transcription request when
recording stops. The `service` in `[provider]` picks where it goes: a vendor,
or `custom`, your server at `url` speaking `format`. Vendors and every format
but `whisper-cpp` are reached through
[rig](https://github.com/0xPlaygrounds/rig), with `url` as the API's base URL,
such as `http://127.0.0.1:8000/v1`; `whisper-cpp` uploads the take to exactly
`url`. A config without `service` keeps working: a URL ending in
`/audio/transcriptions` is an OpenAI-compatible server, and any other URL gets
the whisper.cpp upload. There are no incremental transcript
or text input events during recording. A successful request produces one final
transcript for the whole take.

## Conservative polishing

Polishing is disabled by default, and no model is chosen for it. Enable it on
the GUI's Polish page with the chat model of your choice, or in the config with
the API key in the daemon's environment, never in the config file. A local
server needs no key:

```toml
[polish]
enabled = true
service = "custom"
format = "openai-compatible"   # or any vendor's API: anthropic, gemini, ...
url = "http://127.0.0.1:8080/v1"
model = "qwen3-8b"
timeout_ms = 5000
```

A vendor's own API takes no `url` or `format`, and reads its key from its own
variable unless `api_key_env` names another:

```toml
[polish]
enabled = true
service = "anthropic"   # or openai, gemini, deepseek, mistral, groq, openrouter, xai
model = "your-model"
```

A custom server's URL is its API's base URL, without a query. Remote servers
must use HTTPS; HTTP is allowed only on a loopback address, for example
`http://127.0.0.1:3425/v1`. The request asks for temperature 0
and no reasoning, which each API receives in its own form, or not at all where a
model cannot take it.

Only the non-empty final transcript of each take is considered for correction.
Audio, conversation history, and other takes are not sent to the polishing
service. The prompt only permits conservative ASR correction; translation,
expansion, summaries, answers, and instructions contained in the transcript are
not allowed.

A vendor without a key produces one warning and no HTTP request. Missing keys, timeouts,
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
