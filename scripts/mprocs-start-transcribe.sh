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

./scripts/mprocs-start.sh
sleep 1.2

# Keep this proc UP while recording (attached live mode).
exec target/debug/omni transcribe start
