# Limen

System-wide voice input: hold a key (or toggle it), speak, and the
transcript is committed to the focused app — Ghostty, Claude Code, GTK
apps on Linux/Wayland, any text field on macOS. Nothing is typed while you
speak, and Limen never presses Enter for you.

## Requirements

Linux (Wayland, developed against GNOME/Mutter):

- `pw-record` (PipeWire) and `notify-send` (libnotify)
- Fcitx5 5.1.0+ with its D-Bus addon — it commits Unicode text directly
  instead of simulating keystrokes (`dotool`/`wtype` remain as legacy
  backends)

macOS: no extra packages; recording uses CoreAudio and text commits through
CGEvent.

Both platforms need an ASR service: a local server speaking
OpenAI-compatible `POST /v1/audio/transcriptions` (e.g.
[sglang-omni](https://github.com/sgl-project/sglang-omni) or whisper.cpp),
or an API key for OpenAI, Groq, Mistral, or Gemini.

## Build

```bash
cargo build --release
```

With Nix: `nix build` then `nix run . -- daemon`; `nix develop` enters the
dev shell. The GUI has desktop end-to-end tests on Linux: `cargo gui-e2e`.

## Setup

1. **ASR.** First run writes an annotated config (`$XDG_CONFIG_HOME/limen/config.toml` on Linux,
   `~/Library/Application Support/dev.limen.limen/config.toml` on macOS).
   Pick a provider and model on the GUI's ASR page, or set `provider.url`
   and `provider.model` by hand.
2. **Text input (Linux only).** Install the Fcitx5 addon and restart Fcitx5:
   ```bash
   contrib/fcitx5-limen/install-user.sh
   fcitx5 -d -r
   ```
   Rebuild it after Fcitx upgrades (C++ ABI).
3. **Daemon.** Start it and keep it running — Limen does not install an
   autostart entry:
   ```bash
   limen daemon
   ```
4. **Shortcut.**
   - Linux: `limen shortcut setup portal` (GNOME confirms once; supports
     hold-to-talk) or `setup gnome-keybinding` (toggle only), or bind
     `limen toggle` in your desktop settings yourself. `limen shortcut
     status` shows what is bound.
   - macOS: **global hotkeys are broken for now** — a CLI process never
     receives Carbon hotkey events. Bind `limen toggle` in a launcher
     (Raycast, Hammerspoon, Keyboard Maestro); Hammerspoon can map key
     down/up to `limen hold press` / `limen hold release` for
     hold-to-talk.

macOS asks for two permissions, attributed to the hosting terminal app:
**Microphone** on the first take and **Accessibility** on the first text
commit. `brew install terminal-notifier` gives native-looking
notifications.

## Usage

Toggle: press once to start, again to stop; the take is transcribed and
committed once. Hold-to-talk: speak while held; a hold under 300 ms is
dropped as accidental, one over 60 s is stopped for you.

## GUI control panel

`limen gui` opens the control panel: live dictation state, level meter and
history, ASR/polish provider setup (including ChatGPT and Copilot sign-in),
proxy settings, and the shortcut page. Closing the window never stops the
daemon. API keys and the proxy password are saved in `keys.toml` (mode
0600), never in `config.toml`. `nix run .#headless -- daemon` runs without
the GUI.

## Commands

- `limen gui` — control panel (GUI-enabled builds)
- `limen daemon` — resident process (capture, ASR, text input, shortcuts)
- `limen toggle` — start/stop dictation
- `limen hold press|release` — hold-to-talk key edges (for key-release bindings)
- `limen status` — daemon state (`idle` / `recording` / `finalizing`)
- `limen shortcut status|setup|set|clear|edit` — manage the global shortcuts
- `limen probe <wav>` — transcribe a 16 kHz mono wav and print the result

## Config

The annotated default written on first run covers everything. Highlights:

- `[polish]` (off by default) corrects the final transcript with a chat
  model — conservatively, and audio never leaves the ASR service. If a
  correction cannot be committed, the original transcript is used.
- `[network]` picks the proxy for model requests: `system` (the
  environment's `HTTPS_PROXY` etc.), `none`, or `manual` with `proxy_url`
  and `no_proxy`. Remote servers must use HTTPS; HTTP is loopback-only.
- Logs and the IPC socket live under the XDG state/runtime dirs on Linux
  and under `~/Library/Caches/dev.limen.limen/` on macOS.
