#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

RUNTIME_DIR="${OMNI_RUNTIME_DIR:-$ROOT_DIR/.run}"
if [[ "$RUNTIME_DIR" != /* ]]; then
  RUNTIME_DIR="$ROOT_DIR/$RUNTIME_DIR"
fi
export OMNI_RUNTIME_DIR="$RUNTIME_DIR"
mkdir -p "$OMNI_RUNTIME_DIR"

# Ask daemon to shut down first (handles normal cleanup path, including UI pid lock).
if [[ -x target/debug/omni ]]; then
  target/debug/omni stop --json >/dev/null 2>&1 || true
fi

# Best-effort hard cleanup for stale local dev processes.
pkill -f "target/debug/omni __daemon" >/dev/null 2>&1 || true
pkill -f "target/debug/omni-transcribe-ui" >/dev/null 2>&1 || true
pkill -f "target/debug/omni-ui-tail" >/dev/null 2>&1 || true
pkill -f "cargo run --bin omni-transcribe-ui" >/dev/null 2>&1 || true
pkill -f "cargo run --bin omni-ui-tail" >/dev/null 2>&1 || true

rm -f "$OMNI_RUNTIME_DIR/daemon.sock"
rm -f "$OMNI_RUNTIME_DIR/ui.sock"
rm -f "$OMNI_RUNTIME_DIR/daemon.pid"
rm -f "$OMNI_RUNTIME_DIR/transcribe-ui.pid"

printf '{\n  "ok": true,\n  "action": "stop_all",\n  "runtimeDir": "%s"\n}\n' "$OMNI_RUNTIME_DIR"
