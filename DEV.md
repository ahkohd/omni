# Developer notes

## UI event stream (daemon IPC)

`omni` publishes UI events on `<runtime_dir>/ui.sock` as JSON lines.

For quick inspection, use the separate debug binary:

```bash
omni-ui-tail --wait
# or:
omni-ui-tail --socket /path/to/ui.sock --pretty
```

Full contract and event schema:

- `docs/ui-ipc.md`

## Local UI binaries

Transcription pill UI client:

- `show_ui` hook auto-starts this client on demand (best effort).
- moved window position is persisted as normalized coordinates in
  `<app_data_dir>/omni/transcribe-ui-position.json`
  (`$OMNI_UI_POSITION_DIR` overrides location).

```bash
omni-transcribe-ui
# useful knobs:
omni-transcribe-ui --idle-ms 10000 --hide-ms 250 --retry-ms 220
```

Debug tail client:

```bash
omni-ui-tail --wait
```

## Dev launcher (build + mprocs)

```bash
./scripts/dev-start.sh
```

This script:
- builds all binaries (`cargo build --bins`)
- uses local runtime dir `.run/`
- starts `mprocs` dashboard using `mprocs.yaml`

Recommended dashboard flow:
1. run `start` (it stops all local omni procs first, then builds + starts)
2. run `transcribe-start` / `transcribe-stop`
3. optionally run `ui_tail` for socket events
4. run `stop` when done
