# omni

[![npm version](https://img.shields.io/npm/v/@ahkohd/omni.svg)](https://www.npmjs.com/package/@ahkohd/omni)

A real-time CLI transcription tool.

## Install

```bash
# npm (macOS, Linux, WSL)
npm i -g @ahkohd/omni

# homebrew (macOS, WSL)
brew install ahkohd/tap/omni

# verify
omni --version
```


## Quick start

```bash
omni start
omni doctor
omni transcribe start
# speak, then stop:
omni transcribe stop
omni stop
```

## Backend setup

omni streams audio to any server that speaks the OpenAI Realtime protocol.

Known compatible options:
- [vLLM](https://docs.vllm.ai/) — recommended with [`mistralai/Voxtral-Mini-4B-Realtime-2602`](https://huggingface.co/mistralai/Voxtral-Mini-4B-Realtime-2602):
  ```bash
  vllm serve mistralai/Voxtral-Mini-4B-Realtime-2602 --served-model-name voxtral
  ```
- [LocalAI](https://localai.io/features/openai-realtime/)
- [Speaches](https://speaches.ai/) — OpenAI-compatible realtime server with transcription-only mode
- OpenAI API

### Local example

```bash
omni config set server.baseUrl http://127.0.0.1:8000/v1
omni config set server.model voxtral
```

If you don't use `--served-model-name voxtral`, set the full model id instead:

```bash
omni config set server.model mistralai/Voxtral-Mini-4B-Realtime-2602
```

### Cloud example

```bash
omni config set server.baseUrl https://api.openai.com/v1
omni config set server.apiKey sk-...
omni config set server.model gpt-4o-transcribe
```

## Activation options

Use CLI commands directly, scripts, or your existing launcher/keybind setup.

### skhd

```sh
cmd + alt - r : omni transcribe start --background
cmd + alt - c : omni transcribe stop copy
cmd + alt - i : omni transcribe stop insert
```

### Raycast extension

Extension docs: [Raycast extension README](extensions/raycast/omni/README.md)

### Raycast / Hammerspoon (scripted)

If you prefer your own launcher actions, use one action for start and one for stop mode:

```bash
omni transcribe start --background
omni transcribe stop insert
```

## Hooks

Hooks let you decide what happens during and after transcription. Run built-in actions, your own scripts, or both.

Built-in actions:
- `show_ui` — show transcription UI
- `hide_ui` — hide transcription UI
- `copy` — copy transcript to clipboard
- `paste` — paste into focused app
- `stash` — save current clipboard before overwriting
- `unstash` — restore previously stashed clipboard
- `sleep <ms>` — pause hook execution for N milliseconds

### Hook recipes

```toml
[event.hooks.transcribe]
# default stop
stop = ["hide_ui", "copy"]

# copy-only mode
stop_copy = ["copy"]

# default insert mode (clipboard-safe)
stop_insert = ["hide_ui", "stash", "copy", "paste", "sleep 120", "unstash"]

# custom mode via script
stop_slack = ["./hooks/slack-send.sh"]
```

## Configuration

Config path: `~/.config/omni/config.toml`

```toml
[server]
llmApi = "openai-realtime"
baseUrl = "http://127.0.0.1:8000/v1"
apiKey = ""
model = "voxtral"

[audio]
device = "default"
sample_rate = 16000
channels = 1

[event.hooks.transcribe]
start = ["show_ui"]
stop = ["hide_ui", "copy"]
stop_insert = ["hide_ui", "stash", "copy", "paste", "sleep 120", "unstash"]
```

```bash
omni config path
omni config show
omni config get <dot.key>
omni config set <dot.key> <value>
omni config set <dot.key> '<json>' --json-value
omni config unset <dot.key>
```

## Commands

- `omni start` / `stop` / `status` / `reload` / `doctor` — daemon lifecycle & health checks (`stop` also terminates spawned `omni-transcribe-ui` client)
- `omni transcribe start` — begin recording (`--background` for keybind flows)
- `omni transcribe stop [mode]` — stop and run hooks (`stop copy`, `stop insert`, etc.)
- `omni transcribe status` — recording state, duration, transcript preview
- `omni input show` / `input list` / `input set <id>` / `input set --name "<device>"` — mic selection (`input set` auto-reloads daemon when running)
- `omni config ...` — read/write config

Useful flags:
- `--json` for machine-readable output
- `--debug` / `--debug-json` for live diagnostics

## Troubleshooting

- Run `omni doctor` first.
- If transcription fails, ensure daemon is running: `omni status`.
- If mic input is wrong, inspect and set device:
  - `omni input list`
  - `omni input set <id>`
- On macOS, grant microphone/accessibility permissions when prompted.
- On Linux/WSL, install clipboard helpers for hook builtins:
  - `wl-copy` / `wl-paste` (from `wl-clipboard`) or `xclip` for `copy`/`stash`/`unstash`
  - `wtype` (Wayland) or `xdotool` (X11) for `paste`
- For live diagnostics, use `omni transcribe start --debug`.
