#!/usr/bin/env bash
# Start all plugins then forge.
# Run this from the workspace root (forge-core/).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cleanup() {
    echo "=== Shutting down ==="
    for pid in "$FORGE_PID" "$ROUTER_PID" "$DATA_PID" "$AUTH_PID" "$ECHO_PID"; do
        kill "$pid" 2>/dev/null || true
    done
    wait
}
trap cleanup EXIT

echo "=== Starting plugins ==="
FORGE_LISTEN_ADDR="127.0.0.1:50051" "$ROOT/target/release/forge-plugin-echo-rs" & ECHO_PID=$!
FORGE_LISTEN_ADDR="127.0.0.1:50052" "$ROOT/target/release/forge-plugin-auth-jwt" & AUTH_PID=$!
FORGE_DATA_DB_PATH="/tmp/forge-example.db" FORGE_LISTEN_ADDR="127.0.0.1:50053" "$ROOT/target/release/forge-plugin-data-sqlite" & DATA_PID=$!
FORGE_LISTEN_ADDR="127.0.0.1:50054" "$ROOT/target/release/forge-plugin-http-router" & ROUTER_PID=$!
sleep 2

echo "=== Starting forge ==="
"$ROOT/target/release/forge" run --config "$ROOT/examples/example-backend/forge.toml" & FORGE_PID=$!
wait
