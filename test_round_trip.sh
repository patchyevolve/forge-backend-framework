#!/usr/bin/env bash
set -euo pipefail

# Round-trip integration test: echo-rs plugin through forge run
# Builds everything, starts echo-rs + forge, curls /v1/invoke, cleans up.

ROOT="$(cd "$(dirname "$0")" && pwd)"
TMPDIR="$(mktemp -d)"
cleanup() {
    echo "=== Cleaning up ==="
    kill "$FORGE_PID" 2>/dev/null || true
    kill "$ECHO_PID" 2>/dev/null || true
    rm -rf "$TMPDIR"
    echo "=== Done ==="
}
trap cleanup EXIT

# Build echo-rs and forge-cli
echo "=== Building ==="
cargo build -p forge-plugin-echo-rs -p forge-cli --release 2>&1

# Pick ports
HTTP_PORT=9191
ECHO_PORT=9192

# Create a temp plugin manifest pointing to a TCP address (easier for CI than Unix sockets)
mkdir -p "$TMPDIR/plugins/echo-rs"
cat > "$TMPDIR/plugins/echo-rs/plugin.forge.toml" <<EOF
forge_manifest_version = "1.0"

[plugin]
name = "echo-rs"
version = "0.1.0"
description = "Minimal echo plugin (Rust, Shape A)"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:$ECHO_PORT"

[lifecycle]
restart_policy = "on-failure"
health_check_interval_ms = 500
health_check_failure_threshold = 3
drain_grace_period_ms = 1000

[capabilities]
provides = ["forge.example.echo@1.0"]
requires = []
EOF

# Create forge.toml
cat > "$TMPDIR/forge.toml" <<EOF
forge_config_version = "1.0"

[gateway]
grpc_bind = "127.0.0.1:9190"
http_bind = "127.0.0.1:$HTTP_PORT"

[log]
level = "debug"

[plugins]
manifest_dir = "$TMPDIR/plugins"
EOF

# Start echo-rs
echo "=== Starting echo-rs ==="
FORGE_LISTEN_ADDR="127.0.0.1:$ECHO_PORT" \
    "$ROOT/target/release/forge-plugin-echo-rs" &
ECHO_PID=$!
sleep 1

# Verify echo-rs is listening
if ! kill -0 "$ECHO_PID" 2>/dev/null; then
    echo "ERROR: echo-rs failed to start"
    exit 1
fi

# Start forge
echo "=== Starting forge ==="
FORGE_GATEWAY_HTTP_BIND="127.0.0.1:$HTTP_PORT" \
    "$ROOT/target/release/forge" run --config "$TMPDIR/forge.toml" &
FORGE_PID=$!
sleep 2

# Verify forge is listening
if ! kill -0 "$FORGE_PID" 2>/dev/null; then
    echo "ERROR: forge failed to start"
    exit 1
fi

# Step 1: Check /v1/status shows the plugin
echo "=== Checking /v1/status ==="
STATUS=$(curl -sf http://127.0.0.1:$HTTP_PORT/v1/status)
echo "$STATUS"
if ! echo "$STATUS" | grep -q "echo-rs"; then
    echo "ERROR: echo-rs not found in status"
    exit 1
fi

# Step 2: Round-trip invoke
echo "=== Invoking forge.example.echo ==="
PAYLOAD_B64=$(echo -n "hello" | base64 -w0)
RESPONSE=$(curl -sf -X POST http://127.0.0.1:$HTTP_PORT/v1/invoke \
    -H "Content-Type: application/json" \
    -d "{\"capability\": \"forge.example.echo\", \"payload\": \"$PAYLOAD_B64\"}")
echo "$RESPONSE"

# Verify the response has the uppercased payload ("HELLO" → base64 "SEVMTE8=")
EXPECTED_B64=$(echo -n "HELLO" | base64 -w0)
if ! echo "$RESPONSE" | grep -q "$EXPECTED_B64"; then
    echo "ERROR: expected uppercased echo, got $RESPONSE"
    exit 1
fi

echo ""
echo "=== ROUND-TRIP PASSED ==="
