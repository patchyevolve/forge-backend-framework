#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
RELEASE="$(cd "$ROOT/../.." && pwd)/target/release"
cleanup() {
    echo "=== Shutting down ==="
    for pid in "$FORGE_PID" "$PROCESS_PID" "$INGEST_PID" "$STORE_PID"; do
        kill "$pid" 2>/dev/null || true
    done
    wait
}
trap cleanup EXIT

echo "=== Starting all plugins ==="
FORGE_LISTEN_ADDR="127.0.0.1:51052" "$RELEASE/tutorial-store" & STORE_PID=$!
FORGE_LISTEN_ADDR="127.0.0.1:51051" "$RELEASE/tutorial-ingest" & INGEST_PID=$!
FORGE_LISTEN_ADDR="127.0.0.1:51053" "$RELEASE/tutorial-process" & PROCESS_PID=$!
sleep 2

echo "=== Starting forge ==="
"$RELEASE/forge" run --config "$ROOT/forge.toml" & FORGE_PID=$!

wait
