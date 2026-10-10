# voice-input

System-wide voice input for Wayland: microphone audio streams to a pluggable
ASR backend, and recognized text is typed into whatever input box has focus
(Ghostty, Claude Code, any GUI app).

## Requirements

- Wayland session (developed against GNOME/Mutter)
- `pw-record` (PipeWire), `wtype`, `notify-send` (libnotify)
- An ASR server. Local default: [sglang-omni](https://github.com/sgl-project/sglang-omni)
  serving Qwen3-ASR with `--enable-realtime`.

## Build

```bash
cargo build --release
```

## Setup

1. Start your ASR server, e.g. sgl-omni:
   ```bash
   sgl-omni serve --model-path Qwen/Qwen3-ASR-0.6B --enable-realtime --port 8000
   ```
2. First run writes an annotated default config to
   `$XDG_CONFIG_HOME/voice-input/config.toml`; point `provider.url` at your server.
3. Start the daemon:
   ```bash
   voice-input daemon
   ```
4. Bind a GNOME shortcut (Settings → Keyboard → Custom Shortcuts):
   `voice-input toggle` on a key of your choice (e.g. Super+Z).

Press once to start dictation, again to stop. Only stable text is typed, and
the tool never presses Enter for you.

## Commands

- `voice-input daemon` — resident process (capture, ASR session, injection)
- `voice-input toggle` — start/stop dictation (for the desktop shortcut)
- `voice-input status` — print daemon state (`idle` / `recording` / `finalizing`)
- `voice-input probe <wav>` — stream a 16kHz mono wav and print the provider's
  raw event flow with timestamps

## Providers

| `provider.type` | Protocol | Notes |
|---|---|---|
| `openai-realtime` | WebSocket, sgl-omni realtime flavor | incremental, recommended |
| `openai-batch` | `POST /v1/audio/transcriptions` | buffers a whole take; universal fallback |

## Config

`$XDG_CONFIG_HOME/voice-input/config.toml` — see the annotated default written
on first run. Logs and the IPC socket follow the XDG spec
(`$XDG_STATE_HOME/voice-input/`, `$XDG_RUNTIME_DIR/voice-input.sock`).
