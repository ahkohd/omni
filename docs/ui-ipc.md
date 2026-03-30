# UI IPC Contract

This document defines how external UI clients (e.g. `omni-transcribe-ui`) integrate with the `omni` daemon.

## Runtime paths

Resolve runtime dir in this order:

1. `OMNI_RUNTIME_DIR` (if set and non-empty)
2. `$XDG_RUNTIME_DIR/omni` (if `XDG_RUNTIME_DIR` set)
3. `~/.local/state/omni`

Sockets:

- command socket: `<runtime_dir>/daemon.sock`
- UI event socket: `<runtime_dir>/ui.sock`

## Client startup flow

A UI client should:

1. Resolve runtime dir and socket paths.
2. Fetch an initial snapshot (recommended via command socket `status` / `transcribe_status`, or `omni status --json` fallback).
3. Connect to `ui.sock` and read JSONL events.
4. Merge incoming events into local UI state.
5. On disconnect, reconnect and re-fetch snapshot.

## Event stream format

Each line on `ui.sock` is one JSON object.

Envelope:

```json
{
  "v": 1,
  "seq": 42,
  "at_ms": 1774993680392,
  "type": "audio.energy",
  "payload": {"...": "..."}
}
```

Fields:

- `v`: protocol version (integer)
- `seq`: monotonic sequence number within daemon process lifetime
- `at_ms`: event timestamp (unix epoch ms)
- `type`: event type string
- `payload`: type-specific object

## Current event types

### `ui.show`
Payload:

```json
{ "event": "transcribe.start", "mode": null }
```

### `ui.hide`
Payload:

```json
{ "event": "transcribe.stop", "mode": null }
```

### `transcribe.started`
Payload (synthetic example):

```json
{ "synthetic": true }
```

Payload (realtime example):

```json
{ "synthetic": false, "llm_api": "openai-realtime" }
```

### `transcribe.stopped`
Payload:

```json
{
  "mode": null,
  "duration_ms": 2086,
  "source": "empty",
  "fallback_used": false,
  "transcript": ""
}
```

### `audio.energy`
Payload:

```json
{
  "rms": 0.0113,
  "peak": 0.0380,
  "rms_dbfs": -38.89,
  "peak_dbfs": -28.39,
  "silent": false,
  "silence_threshold": 0.008,
  "chunks": 10,
  "samples": 5120
}
```

### `transcript.delta`
Payload:

```json
{ "delta": "hello", "preview": "hello world" }
```

### `transcript.snapshot`
Payload:

```json
{ "text": "hello world" }
```

## Delivery semantics

- Broadcast, best-effort.
- No replay and no ACK.
- Slow/broken clients may be dropped.
- `seq` resets when daemon restarts.

UI clients must tolerate missed events and recover by reconnect + snapshot.

## Reconnect strategy

Recommended reconnect loop:

- detect EOF / socket error
- backoff retry (e.g. 100ms -> 250ms -> 500ms, cap 1s)
- after reconnect, refresh snapshot before resuming rendering

## Control responsibilities

UI clients should send lifecycle actions through command IPC (`daemon.sock`) or by invoking `omni` commands.
The UI event socket is read-only.

## Reference client + debug tools

`show_ui` builtin behavior in `omni` daemon ensures the default `omni-transcribe-ui` client is started (best effort) and then emits `ui.show`.

Transcription pill client:

```bash
omni-transcribe-ui
omni-transcribe-ui --idle-ms 10000 --hide-ms 250 --retry-ms 220
```

`omni-transcribe-ui` persists moved window position in
`<app_data_dir>/omni/transcribe-ui-position.json` as normalized center
coordinates (`x`,`y` in `[0,1]`). `$OMNI_UI_POSITION_DIR` overrides the
directory. On startup, those coordinates are mapped onto the active visible
display bounds so placement survives resolution/monitor changes.

Debugging is provided by a separate binary so core `omni` command surface stays minimal:

```bash
omni-ui-tail --wait
omni-ui-tail --pretty
omni-ui-tail --socket /path/to/ui.sock
```
