#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
TMPDIR="$(mktemp -d)"

cleanup() {
    echo "=== Cleaning up ==="
    kill "$FORGE_PID" 2>/dev/null || true
    kill "$ECHO_PID" 2>/dev/null || true
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

# Build release (faster startup than debug)
cargo build -p forge-plugin-echo-rs -p forge-cli --release 2>&1 | tail -5

# Copy the committed manifest into a temp dir so we don't modify the repo
cp -r "$ROOT/plugins" "$TMPDIR/"

# Create a forge.toml pointing at the copied manifest dir
cat > "$TMPDIR/forge.toml" <<EOF
forge_config_version = "1.0"

[gateway]
http_bind = "127.0.0.1:9191"

[log]
level = "info"

[plugins]
manifest_dir = "$TMPDIR/plugins"
EOF

# Start echo-rs with NO env var — main.rs hardcodes FORGE_LISTEN_ADDR to match manifest
echo "=== Starting echo-rs (plain, no FORGE_LISTEN_ADDR set) ==="
"$ROOT/target/release/forge-plugin-echo-rs" &
ECHO_PID=$!
sleep 1

if ! kill -0 "$ECHO_PID" 2>/dev/null; then
    echo "ERROR: echo-rs failed to start"
    exit 1
fi

# Start forge
echo "=== Starting forge ==="
"$ROOT/target/release/forge" run --config "$TMPDIR/forge.toml" &
FORGE_PID=$!
sleep 2

if ! kill -0 "$FORGE_PID" 2>/dev/null; then
    echo "ERROR: forge failed to start"
    exit 1
fi

# Status check
echo "=== /v1/status ==="
curl -sf http://127.0.0.1:9191/v1/status | python3 -m json.tool

# Round-trip invoke
echo "=== Invoke ==="
PAYLOAD_B64=$(echo -n "hello" | base64 -w0)
RESPONSE=$(curl -sf -X POST http://127.0.0.1:9191/v1/invoke \
    -H "Content-Type: application/json" \
    -d "{\"capability\": \"forge.example.echo\", \"payload\": \"$PAYLOAD_B64\"}")
echo "$RESPONSE" | python3 -m json.tool

EXPECTED_B64=$(echo -n "HELLO" | base64 -w0)
if ! echo "$RESPONSE" | grep -q "$EXPECTED_B64"; then
    echo "FAIL: expected $EXPECTED_B64, got $RESPONSE"
    exit 1
fi

echo ""
echo "=== ROUND-TRIP PASSED (committed files) ==="
