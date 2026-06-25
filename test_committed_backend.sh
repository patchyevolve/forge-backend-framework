#!/usr/bin/env bash
# Runs 8 integration tests against the committed example-backend configs.
# No generated manifests, no env overrides — plugins use their committed addresses.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
TMPDIR="$(mktemp -d)"
cleanup() {
    echo "=== Cleaning up ==="
    for pid in "$FORGE_PID" "$ROUTER_PID" "$DATA_PID" "$AUTH_PID" "$ECHO_PID"; do
        kill "$pid" 2>/dev/null || true
    done
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

# --- Helper: fire off an RPC and decode the response ---
invoke() {
    local cap="$1" payload="$2"
    local encoded=$(echo -n "$payload" | base64 -w0)
    curl -sf -X POST http://127.0.0.1:9091/v1/invoke \
        -H "Content-Type: application/json" \
        -d "{\"capability\": \"$cap\", \"payload\": \"$encoded\"}"
}
decode() {
    python3 -c "import sys,json,base64; d=json.loads('$1'); print(base64.b64decode(d['payload']).decode())" 2>/dev/null
}

# Start every plugin without FORGE_LISTEN_ADDR overrides —
# they'll bind to whatever address their committed plugin.forge.toml says.
echo "=== Starting plugins (no FORGE_LISTEN_ADDR overrides) ==="
"$ROOT/target/release/forge-plugin-echo-rs" & ECHO_PID=$!
"$ROOT/target/release/forge-plugin-auth-jwt" & AUTH_PID=$!
FORGE_DATA_DB_PATH="$TMPDIR/test.db" "$ROOT/target/release/forge-plugin-data-sqlite" & DATA_PID=$!
"$ROOT/target/release/forge-plugin-http-router" & ROUTER_PID=$!
sleep 2

# Start forge using the example-backend config from the repo
echo "=== Starting forge (examples/example-backend/forge.toml) ==="
"$ROOT/target/release/forge" run --config "$ROOT/examples/example-backend/forge.toml" & FORGE_PID=$!
sleep 3

echo "=== /v1/status ==="
curl -sf http://127.0.0.1:9091/v1/status | python3 -m json.tool

echo ""
echo "=== Test 1: echo-rs ==="
DEC=$(decode "$(invoke "forge.example.echo" "hello")")
echo "  -> $DEC"
[ "$DEC" = "HELLO" ] || { echo "FAIL"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 2: auth-jwt (valid token) ==="
DEC=$(decode "$(invoke "forge.auth.verify" '{"token":"forge-demo-secret"}')")
echo "  -> $DEC"
echo "$DEC" | grep -q '"valid".*true' || { echo "FAIL"; exit 1; }
echo "  PASS"

echo "=== Test 2b: auth-jwt (invalid token) ==="
DEC=$(decode "$(invoke "forge.auth.verify" '{"token":"wrong"}')")
echo "  -> $DEC"
echo "$DEC" | grep -q '"valid".*false' || { echo "FAIL"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 3: data-sqlite setup + write ==="
DEC=$(decode "$(invoke "forge.data.write" '{"sql":"CREATE TABLE IF NOT EXISTS users (id INTEGER, name TEXT, email TEXT)"}')")
echo "  Create: $DEC"
DEC=$(decode "$(invoke "forge.data.write" '{"sql":"INSERT INTO users (id, name, email) VALUES (?1, ?2, ?3)", "params":[1, "Alice", "alice@example.com"]}')")
echo "  Insert: $DEC"
echo "$DEC" | grep -q '"rows_affected":1' || { echo "FAIL (expected rows_affected=1)"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 3b: data-sqlite query ==="
DEC=$(decode "$(invoke "forge.data.query" '{"sql":"SELECT id, name, email FROM users WHERE id=?1", "params":[1]}')")
echo "  -> $DEC"
echo "$DEC" | grep -q "Alice" || { echo "FAIL"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 4: http-router full chain ==="
DEC=$(decode "$(invoke "forge.http.route" '{"method":"GET","path":"/users","headers":{"authorization":"Bearer forge-demo-secret"},"body":""}')")
echo "  -> $DEC"
echo "$DEC" | grep -q '"status".*200' || { echo "FAIL (not 200)"; exit 1; }
echo "$DEC" | grep -q "Alice" || { echo "FAIL (no Alice)"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 5: http-router bad auth -> 401 ==="
DEC=$(decode "$(invoke "forge.http.route" '{"method":"GET","path":"/users","headers":{"authorization":"Bearer wrong-token"},"body":""}')")
echo "  -> $DEC"
echo "$DEC" | grep -q '401' || { echo "FAIL"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 6: http-router no auth -> 401 ==="
DEC=$(decode "$(invoke "forge.http.route" '{"method":"GET","path":"/users","headers":{},"body":""}')")
echo "  -> $DEC"
echo "$DEC" | grep -q '401' || { echo "FAIL (expected 401)"; exit 1; }
echo "$DEC" | grep -q "Authorization header required" || { echo "FAIL (expected specific message)"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 7: http-router POST /users with params ==="
DEC=$(decode "$(invoke "forge.http.route" '{"method":"POST","path":"/users","headers":{"authorization":"Bearer forge-demo-secret"},"body":"{\"name\":\"Bob\",\"email\":\"bob@example.com\"}"}')")
echo "  -> $DEC"
echo "$DEC" | grep -q '"status".*200' || { echo "FAIL (not 200)"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 8: http-router 404 ==="
DEC=$(decode "$(invoke "forge.http.route" '{"method":"DELETE","path":"/nonexistent","headers":{"authorization":"Bearer forge-demo-secret"},"body":""}')")
echo "  -> $DEC"
echo "$DEC" | grep -q '404' || { echo "FAIL"; exit 1; }
echo "  PASS"

echo ""
echo "=== ALL 8 TESTS PASSED ==="
echo "Summary: echo | auth(valid/invalid) | data(params/write/query) | router chain | bad-auth-401 | no-auth-401 | POST | 404"
