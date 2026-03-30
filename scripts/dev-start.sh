#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

RUNTIME_DIR="${OMNI_RUNTIME_DIR:-$ROOT_DIR/.run}"
export OMNI_RUNTIME_DIR="$RUNTIME_DIR"
mkdir -p "$OMNI_RUNTIME_DIR"

echo "[omni] building all binaries..."
cargo build --bins

if [[ "${1:-}" == "--build-only" ]]; then
  echo "[omni] build complete (build-only mode)."
  exit 0
fi

if ! command -v mprocs >/dev/null 2>&1; then
  echo "[omni] mprocs is required for this script." >&2
  echo "Install: brew install mprocs" >&2
  exit 1
fi

echo "[omni] runtime dir: $OMNI_RUNTIME_DIR"
echo "[omni] launching mprocs..."

if mprocs --help 2>&1 | grep -q -- "--config"; then
  exec mprocs --config "$ROOT_DIR/mprocs.yaml"
fi

exec mprocs -c "$ROOT_DIR/mprocs.yaml"
