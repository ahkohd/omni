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

# Only stop & insert if transcription is currently running.
if target/debug/omni transcribe status --json 2>/dev/null | grep -Eq '"recording"[[:space:]]*:[[:space:]]*true'; then
  target/debug/omni transcribe stop insert >/dev/null 2>&1 || true
fi
