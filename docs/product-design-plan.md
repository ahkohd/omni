# omni CLI — Product Design Plan (Draft v0.3)

## 1) Product Summary

**omni** is a tiny, elegant, keybind-first CLI for real-time transcription against a running vLLM Omni/Voxtral server.

Core promise: press a key to start recording, press a key to stop, and route transcript text through simple, composable hook primitives.

---

## 2) Design Principles

1. **Tiny surface area**: minimal commands, predictable behavior.
2. **Fast keybind loop**: all commands safe to call repeatedly from `skhd`.
3. **BYO triggers**: user composes behavior with hooks.
4. **Scriptable first**: stable output, clear exit codes, optional `--json`.
5. **No hidden side effects**: primitives do exactly one thing.
6. **UI-ready hooks**: `show_ui` ensures the default UI client is running and emits `ui.show`; `hide_ui` emits `ui.hide`.

---

## 3) Inspiration from Yagami (applied)

From `ahkohd/yagami`, adopt:

1. **Lifecycle symmetry**: `start` / `stop` / `status`.
2. **Daemon + thin CLI split**: instant CLI dispatch, daemon owns runtime state.
3. **Ops-focused UX**: optional `doctor`, optional `reload`.
4. **Scriptability flags**: `--json` first, optional `--profile` later.
5. **Config clarity**: `flags > env > config > defaults`.

---

## 4) Goals and Non-Goals

### Goals (MVP)
- Daemon lifecycle:
  - `omni start`
  - `omni stop`
  - `omni status`
- Transcription lifecycle:
  - `omni transcribe start`
  - `omni transcribe status`
  - `omni transcribe stop`
  - `omni transcribe stop <mode>` (user-defined)
- Real-time transcription streaming.
- Hook system with composable builtins + scripts.
- Stable behavior under repeated global hotkey invocations.

### Non-Goals (MVP)
- Rich TUI/GUI.
- Multi-user orchestration.
- Cloud auth/account flows.
- Heavy post-processing pipeline framework.

---

## 5) Target User Flows

### A) Stop without mode
1. `omni transcribe start`
2. speak
3. `omni transcribe stop`
4. run `stop` hook only

### B) Stop with mode
1. `omni transcribe start`
2. speak
3. `omni transcribe stop insert`
4. run `stop_insert` hook only

### C) Custom mode
1. `omni transcribe stop slack`
2. run `stop_slack`

---

## 6) CLI Contract

## Commands
```bash
omni start
omni stop
omni status

omni input show
omni input list
omni input set <id>
omni input set --name "<device>"

omni transcribe start
omni transcribe start --background # alias: --bg
omni transcribe start --debug
omni transcribe start --debug-json
omni transcribe status
omni transcribe stop
omni transcribe stop <mode>
```

Examples:
```bash
omni transcribe stop copy
omni transcribe stop insert
omni transcribe stop slack
```

## Optional commands
```bash
omni doctor
omni reload

# config management (Yagami-style)
omni config path
omni config show
omni config get <key>
omni config set <key> <value>
omni config unset <key>
```

`reload` should validate and re-apply runtime config without restarting keybind workflows.

## Suggested global flags
- `--json` (status/doctor/scriptable output)
- `--profile` (optional timing breakdown)

## Behavioral rules
- `start` when already running: success (idempotent no-op).
- `stop` when stopped: success (idempotent no-op) and best-effort UI client cleanup.
- `input show` prints current configured input and active/default resolution.
- `input set <id>` and `input set --name "<device>"` persist `audio.device` in config and auto-reload running daemon config.
- `transcribe start` while recording: non-fatal no-op + warning.
- `transcribe start` is live-attached by default (streams transcript preview until stop).
- `transcribe start --background` (or `--bg`) disables foreground live attachment.
- `transcribe start --debug` prints transport counters plus mic/input level diagnostics (RMS/peak/silence).
- `transcribe start --debug-json` emits compact realtime event JSON lines plus `audio_meter` updates.
- `transcribe stop --json` includes transcript resolution metadata (source + fallback attempted/used/error).
- `doctor --json` includes a short mic probe with capture + level diagnostics (RMS/peak/silence).
- `transcribe stop ...` while idle or daemon-offline: idempotent success/no-op.
- `transcribe status` reports recording state + elapsed duration + transcript preview (if available).
- `transcribe stop` resolves to `event.hooks.transcribe.stop`.
- `transcribe stop <mode>` resolves to `event.hooks.transcribe.stop_<mode>`.
- **No inheritance**: `transcribe.stop` and `transcribe.stop_*` are independent.

---

## 7) Runtime Architecture

## Components
1. **CLI frontend (clap)** — parse + dispatch.
2. **Daemon** — state machine, capture, streaming, hook execution.
3. **Audio engine (cpal)** — microphone capture + normalization.
4. **Transcription client** — stream to server, receive partial + final text.
5. **Hook runner** — run builtins/scripts in ordered chain.
6. **Clipboard adapter (platform)** — used by `stash/copy/paste/unstash` builtins.

## Control transport
- Primary command socket: local Unix socket (or named pipe) for request/response control.
- Secondary UI event socket: local Unix socket (`ui.sock`) streaming JSON-line UI events (`audio.energy`, `transcript.delta`, `ui.show`, etc.).
- Single-instance lock/PID.

## Optional HTTP control plane (post-MVP)
- `GET /health`, `POST /stats`, `POST /reload`, `POST /stop`, etc.

---

## 8) State Model

States:
- `Stopped`
- `Idle`
- `Recording`
- `Finalizing`

Transitions:
- `start` → `Idle`
- `transcribe start` (`Idle`) → `Recording`
- `transcribe stop [mode]` (`Recording`) → `Finalizing` → `Idle`
- `stop` (`Idle`) → `Stopped`

---

## 9) Configuration (`config.toml`)

## File location
- `~/.config/omni/config.toml`

## Precedence
1. CLI flags
2. Environment variables (`OMNI_*`)
3. `config.toml`
4. Built-in defaults

## Config CLI (recommended)
```bash
omni config path
omni config show
omni config get server.url
omni config set server.url http://127.0.0.1:8000
omni config set audio.sample_rate 16000 --json-value
omni config set event.hooks.transcribe.stop '["hide_ui","copy"]' --json-value
omni config unset event.hooks.transcribe.stop_insert
```

Notes:
- Keys use dot notation.
- `--json-value` parses numbers/booleans/arrays/objects safely.
- Without `--json-value`, values are treated as strings.

## Minimal schema
```toml
[server]
llmApi = "openai-realtime" # default, and only supported implementation in v1
baseUrl = "http://127.0.0.1:8000/v1"
apiKey = ""
model = "voxtral"

[audio]
device = "default"
sample_rate = 16000
channels = 1

[event.hooks.transcribe]
start = ["show_ui"]

# stop without mode
stop = ["hide_ui", "copy"]

# stop with modes
stop_copy = ["copy"]
stop_insert = ["hide_ui", "stash", "copy", "paste", "unstash"]
stop_slack = ["./hooks/slack-send.sh"]
```

Notes:
- Any `event.hooks.transcribe.stop_<mode>` key is valid.
- `event.hooks.transcribe.stop` and `event.hooks.transcribe.stop_<mode>` do not inherit from each other.
- `llmApi=openai-realtime` keeps backend compatibility broad (not tied to vLLM only).
- Any server implementing compatible OpenAI-realtime semantics can be used via `baseUrl`.

---

## 10) Hook/Trigger Semantics

## Builtin actions (MVP)
- `show_ui` (ensure default `omni-transcribe-ui` client is running, then emit `ui.show`)
- `hide_ui` (emit `ui.hide` over Unix IPC)
- `stash`
- `copy`
- `paste`
- `unstash`

## Primitive behavior
- `stash`: save current clipboard snapshot.
- `copy`: copy finalized transcript to clipboard (**no hidden stash**).
- `paste`: issue paste into focused app.
- `unstash`: restore prior stashed clipboard.

Operational rules:
- `unstash` is no-op if no stash exists.
- single-slot stash is sufficient for MVP.
- `unstash` clears stash after successful attempt.

## External actions
- Any other string is executed as a command/script.

## Execution policy
- string or array supported.
- run in listed order.
- per-action timeout (e.g., 2s default).
- hook failures are logged and surfaced; they do not crash daemon.

## Hook env (proposal)
- `OMNI_EVENT`
- `OMNI_MODE` (if stop mode used)
- `OMNI_TRANSCRIPT`
- `OMNI_TRANSCRIPT_PATH` (optional)
- `OMNI_TIMESTAMP`

---

## 11) Keybind Integration (skhd)

```sh
cmd + alt - r : omni transcribe start --background
cmd + alt - c : omni transcribe stop copy
cmd + alt - i : omni transcribe stop insert
cmd + alt - s : omni transcribe stop slack
```

---

## 12) Reliability, Security, Observability

- Single-instance daemon lock.
- Graceful signal shutdown.
- Structured logs.
- Avoid shell eval for builtins.
- No raw audio retention unless explicitly enabled.
- macOS paste automation requires Accessibility/Automation permissions.

---

## 13) Tech Stack

- Rust
- clap
- cpal
- tokio (recommended)

---

## 14) MVP Milestones

1. **M1**: daemon + lifecycle (`start/stop/status`) + config CLI (`config path/show/get/set/unset`)
2. **M2**: transcription path (`transcribe start`, `transcribe stop`)
3. **M3**: mode-based stop hooks (`transcribe stop <mode>`)
4. **M4**: primitives (`stash/copy/paste/unstash`) + UI IPC control builtins
5. **M5**: scriptability polish (`--json`, docs, robust errors)

---

## 15) Open Questions

1. For unknown `event.hooks.transcribe.stop_<mode>`, return hard error or no-op warning?
2. Should `paste` include configurable settle delay (e.g., `paste_delay_ms`)?
3. Should `unstash` run automatically on hook failure via finally semantics?
4. Do we need multi-slot stash, or is single-slot enough?
5. Is `doctor` in MVP or v1.1?

---

## 16) Definition of Done (MVP)

- Keybind-driven loop is stable under repeated use.
- `transcribe stop <mode>` works with arbitrary user-defined modes.
- Primitive hooks behave exactly as documented (no hidden behavior).
- `event.hooks.transcribe.stop` and `event.hooks.transcribe.stop_*` are independent and predictable.
- Latency and transcript quality are acceptable with existing server.
