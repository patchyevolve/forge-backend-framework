#!/usr/bin/env bash
# shellcheck disable=SC2317
set -euo pipefail

# Integration tests for HTTP gateway features: CORS, rate limiting, structured errors.
# Requires forge-cli and forge-plugin-echo-rs to be built first.

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

HTTP_PORT=9291
ECHO_PORT=9292

# Create forge config with CORS enabled
cat > "$TMPDIR/forge.toml" <<EOF
forge_config_version = "1.0"

[gateway]
grpc_bind = "127.0.0.1:9290"
http_bind = "127.0.0.1:${HTTP_PORT}"
cors_allowed_origins = ["*"]
rate_limit_per_minute = 5
max_body_size = 65536

[log]
level = "info"

[plugins]
manifest_dir = "plugins"
watch = false
EOF

# Plugin manifest
mkdir -p "$TMPDIR/plugins/echo-rs"
cat > "$TMPDIR/plugins/echo-rs/plugin.forge.toml" <<EOF
forge_manifest_version = "1.0"

[plugin]
name = "echo-rs"
version = "0.1.0"
description = "Echo plugin"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:${ECHO_PORT}"

[lifecycle]
restart_policy = "never"

[capabilities]
provides = ["forge.example.echo@1.0"]
requires = []
EOF

# Build
echo "=== Building ==="
cargo build -p forge-cli -p forge-plugin-echo-rs --release 2>&1

# Start echo plugin
FORGE_LISTEN_ADDR="127.0.0.1:${ECHO_PORT}" \
    "$ROOT/target/release/forge-plugin-echo-rs" &
ECHO_PID=$!
sleep 1

# Start forge
FORGE_LOG_LEVEL=info "$ROOT/target/release/forge" run --config "$TMPDIR/forge.toml" &
FORGE_PID=$!
sleep 2

# ---- Test 1: CORS headers ------------------------------------------------
echo "=== Test 1: CORS headers ==="
CORS_RESULT=$(curl -s -i -X OPTIONS \
    -H "Origin: https://example.com" \
    -H "Access-Control-Request-Method: POST" \
    "http://127.0.0.1:${HTTP_PORT}/v1/invoke" 2>&1 | head -20)
if echo "$CORS_RESULT" | grep -q "access-control-allow-origin"; then
    echo "PASS: CORS headers present"
else
    echo "FAIL: CORS headers missing"
    echo "$CORS_RESULT"
    exit 1
fi

# ---- Test 2: Health check ------------------------------------------------
echo "=== Test 2: Health check ==="
HEALTH=$(curl -s "http://127.0.0.1:${HTTP_PORT}/v1/healthz")
if echo "$HEALTH" | grep -q '"status":"ok"'; then
    echo "PASS: Health check ok"
else
    echo "FAIL: Health check failed: $HEALTH"
    exit 1
fi

# ---- Test 3: Normal invoke ------------------------------------------------
echo "=== Test 3: Normal invoke ==="
PAYLOAD=$(echo -n "hello world" | base64 -w0)
INVOKE=$(curl -s -X POST "http://127.0.0.1:${HTTP_PORT}/v1/invoke" \
    -H "Content-Type: application/json" \
    -d "{\"capability\":\"forge.example.echo\",\"payload\":\"${PAYLOAD}\"}")
REQUEST_ID=$(echo "$INVOKE" | python3 -c "import sys,json;print(json.load(sys.stdin).get('request_id',''))" 2>/dev/null || echo "")
if [ -n "$REQUEST_ID" ]; then
    echo "PASS: Invoke returned request_id: $REQUEST_ID"
else
    echo "FAIL: No request_id in response: $INVOKE"
    exit 1
fi

# ---- Test 4: Invalid base64 payload ---------------------------------------
echo "=== Test 4: Invalid base64 payload ==="
ERR=$(curl -s -X POST "http://127.0.0.1:${HTTP_PORT}/v1/invoke" \
    -H "Content-Type: application/json" \
    -d '{"capability":"forge.example.echo","payload":"not-valid!!!"}')
if echo "$ERR" | grep -q '"error"'; then
    echo "PASS: Invalid payload rejected"
else
    echo "FAIL: Expected error for invalid payload: $ERR"
    exit 1
fi

# ---- Test 5: Unknown capability -------------------------------------------
echo "=== Test 5: Unknown capability ==="
NOT_FOUND=$(curl -s -o /dev/null -w "%{http_code}" -X POST "http://127.0.0.1:${HTTP_PORT}/v1/invoke" \
    -H "Content-Type: application/json" \
    -d '{"capability":"does.not.exist","payload":""}')
if [ "$NOT_FOUND" = "404" ]; then
    echo "PASS: Unknown capability returns 404"
else
    echo "FAIL: Expected 404, got $NOT_FOUND"
    exit 1
fi

# ---- Test 6: Rate limiting -------------------------------------------------
echo "=== Test 6: Rate limiting ==="
RATE_LIMITED=0
for _ in $(seq 1 10); do
    CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
        "http://127.0.0.1:${HTTP_PORT}/v1/invoke" \
        -H "Content-Type: application/json" \
        -d "{\"capability\":\"forge.example.echo\",\"payload\":\"$(echo -n test | base64 -w0)\"}")
    if [ "$CODE" = "429" ]; then
        RATE_LIMITED=1
        echo "PASS: Rate limiting kicked in (429)"
        break
    fi
done
if [ "$RATE_LIMITED" -eq 0 ]; then
    echo "FAIL: Rate limiting did not trigger (limit=5/min, made 10 requests)"
    exit 1
fi

# ---- Test 7: Status endpoint ------------------------------------------------
echo "=== Test 7: Status endpoint ==="
STATUS=$(curl -s "http://127.0.0.1:${HTTP_PORT}/v1/status")
if echo "$STATUS" | grep -q "echo-rs"; then
    echo "PASS: Status shows echo-rs plugin"
else
    echo "FAIL: Status missing echo-rs: $STATUS"
    exit 1
fi

# ---- Test 8: Body size limit ------------------------------------------------
echo "=== Test 8: Body size limit ==="
# The config sets max_body_size = 65536 (64KB). Send a 70KB payload.
LARGE_PAYLOAD=$(python3 -c "print('A'*70000)" | base64 -w0)
BODY_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    "http://127.0.0.1:${HTTP_PORT}/v1/invoke" \
    -H "Content-Type: application/json" \
    -d "{\"capability\":\"forge.example.echo\",\"payload\":\"${LARGE_PAYLOAD}\"}")
if [ "$BODY_CODE" = "413" ]; then
    echo "PASS: Body size limit enforced (413)"
else
    echo "FAIL: Expected 413 for oversized body, got $BODY_CODE"
    exit 1
fi

# ---- Stop HTTP forge, start TLS forge ---------------------------------------
echo "=== Switching to TLS mode ==="
kill "$FORGE_PID" 2>/dev/null || true
wait "$FORGE_PID" 2>/dev/null || true

TLS_PORT=9293
GRPC_PORT=9294

# Generate self-signed cert
CERT_DIR="$TMPDIR/tls"
mkdir -p "$CERT_DIR"
openssl req -x509 -newkey rsa:2048 -keyout "$CERT_DIR/key.pem" \
    -out "$CERT_DIR/cert.pem" -days 365 -nodes -subj "/CN=localhost" 2>&1

# Create TLS config
cat > "$TMPDIR/forge-tls.toml" <<EOF
forge_config_version = "1.0"

[gateway]
grpc_bind = "127.0.0.1:${GRPC_PORT}"
http_bind = "127.0.0.1:${TLS_PORT}"
tls = true
tls_cert_path = "${CERT_DIR}/cert.pem"
tls_key_path = "${CERT_DIR}/key.pem"
cors_allowed_origins = ["*"]

[log]
level = "info"

[plugins]
manifest_dir = "plugins"
watch = false
EOF

# Start forge with TLS
FORGE_LOG_LEVEL=info "$ROOT/target/release/forge" run \
    --config "$TMPDIR/forge-tls.toml" &
FORGE_PID=$!
sleep 3

# ---- Test 9: TLS health check ------------------------------------------------
echo "=== Test 9: TLS health check ==="
HEALTH=$(curl -s -k "https://127.0.0.1:${TLS_PORT}/v1/healthz")
if echo "$HEALTH" | grep -q '"status":"ok"'; then
    echo "PASS: TLS health check ok"
else
    echo "FAIL: TLS health check failed: $HEALTH"
    exit 1
fi

# ---- Test 10: TLS invoke -----------------------------------------------------
echo "=== Test 10: TLS invoke ==="
PAYLOAD=$(echo -n "hello-tls" | base64 -w0)
INVOKE=$(curl -s -k -X POST "https://127.0.0.1:${TLS_PORT}/v1/invoke" \
    -H "Content-Type: application/json" \
    -d "{\"capability\":\"forge.example.echo\",\"payload\":\"${PAYLOAD}\"}")
REQUEST_ID=$(echo "$INVOKE" | python3 -c "import sys,json;print(json.load(sys.stdin).get('request_id',''))" 2>/dev/null || echo "")
if [ -n "$REQUEST_ID" ]; then
    echo "PASS: TLS invoke returned request_id: $REQUEST_ID"
else
    echo "FAIL: TLS invoke failed: $INVOKE"
    exit 1
fi

# ---- Test 11: TLS CORS -------------------------------------------------------
echo "=== Test 11: TLS CORS ==="
CORS_RESULT=$(curl -s -k -i -X OPTIONS \
    -H "Origin: https://example.com" \
    -H "Access-Control-Request-Method: POST" \
    "https://127.0.0.1:${TLS_PORT}/v1/invoke" 2>&1 | head -20)
if echo "$CORS_RESULT" | grep -q "access-control-allow-origin"; then
    echo "PASS: TLS CORS headers present"
else
    echo "FAIL: TLS CORS headers missing"
    echo "$CORS_RESULT"
    exit 1
fi

echo ""
echo "=== All gateway feature tests passed ==="
