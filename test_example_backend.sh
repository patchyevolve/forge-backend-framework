#!/usr/bin/env bash
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

echo "=== Building ==="
cargo build -p forge-plugin-echo-rs \
    -p forge-plugin-auth-jwt \
    -p forge-plugin-data-sqlite \
    -p forge-plugin-http-router \
    -p forge-cli --release 2>&1 | tail -3

# --- Setup manifests using Python for reliable quoting ---
python3 -c "
import os, json
d = '$TMPDIR'
plugins = {
    'echo-rs': ('50051', ['forge.example.echo@1.0']),
    'auth-jwt': ('50052', ['forge.auth.verify@1.0']),
    'data-sqlite': ('50053', ['forge.data.query@1.0', 'forge.data.write@1.0']),
    'http-router': ('50054', ['forge.http.route@1.0']),
}
for name, (port, provides) in plugins.items():
    pdir = os.path.join(d, 'plugins', name)
    os.makedirs(pdir, exist_ok=True)
    with open(os.path.join(pdir, 'plugin.forge.toml'), 'w') as f:
        f.write(f'''forge_manifest_version = \"1.0\"

[plugin]
name = \"{name}\"
version = \"0.1.0\"
description = \"{name} plugin\"
protocol_version = \"1.0\"

[transport]
shape = \"server\"
address = \"http://127.0.0.1:{port}\"

[lifecycle]
restart_policy = \"on-failure\"
health_check_interval_ms = 1000
health_check_failure_threshold = 3
drain_grace_period_ms = 1000

[capabilities]
provides = {json.dumps(provides)}
requires = []
''')
with open(os.path.join(d, 'forge.toml'), 'w') as f:
    f.write(f'''forge_config_version = \"1.0\"

[gateway]
grpc_bind = \"127.0.0.1:9190\"
http_bind = \"127.0.0.1:9191\"

[log]
level = \"info\"

[plugins]
manifest_dir = \"{d}/plugins\"
''')
"

# --- Helper: invoke and decode ---
invoke() {
    local cap="$1" payload="$2"
    local encoded=$(echo -n "$payload" | base64 -w0)
    curl -sf -X POST http://127.0.0.1:9191/v1/invoke \
        -H "Content-Type: application/json" \
        -d "{\"capability\": \"$cap\", \"payload\": \"$encoded\"}"
}

decode() {
    python3 -c "import sys,json,base64; d=json.loads('$1'); print(base64.b64decode(d['payload']).decode())" 2>/dev/null
}

# --- Start plugins ---
echo "=== Starting plugins ==="
FORGE_LISTEN_ADDR="127.0.0.1:50051" "$ROOT/target/release/forge-plugin-echo-rs" & ECHO_PID=$!
FORGE_LISTEN_ADDR="127.0.0.1:50052" "$ROOT/target/release/forge-plugin-auth-jwt" & AUTH_PID=$!
FORGE_DATA_DB_PATH="$TMPDIR/test.db" FORGE_LISTEN_ADDR="127.0.0.1:50053" "$ROOT/target/release/forge-plugin-data-sqlite" & DATA_PID=$!
FORGE_LISTEN_ADDR="127.0.0.1:50054" "$ROOT/target/release/forge-plugin-http-router" & ROUTER_PID=$!
sleep 2

echo "=== Starting forge ==="
"$ROOT/target/release/forge" run --config "$TMPDIR/forge.toml" & FORGE_PID=$!
sleep 3

echo "=== /v1/status ==="
curl -sf http://127.0.0.1:9191/v1/status | python3 -m json.tool

echo ""
echo "=== Test 1: echo-rs ==="
RESP=$(invoke "forge.example.echo" "hello")
DEC=$(decode "$RESP")
echo "  -> $DEC"
[ "$DEC" = "HELLO" ] || { echo "FAIL"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 2: auth-jwt (valid token) ==="
RESP=$(invoke "forge.auth.verify" '{"token":"forge-demo-secret"}')
DEC=$(decode "$RESP")
echo "  -> $DEC"
echo "$DEC" | grep -q '"valid".*true' || { echo "FAIL"; exit 1; }
echo "  PASS (valid)"

echo "=== Test 2b: auth-jwt (invalid token) ==="
RESP=$(invoke "forge.auth.verify" '{"token":"wrong"}')
DEC=$(decode "$RESP")
echo "  -> $DEC"
echo "$DEC" | grep -q '"valid".*false' || { echo "FAIL"; exit 1; }
echo "  PASS (invalid)"

echo ""
echo "=== Test 3: data-sqlite write ==="
RESP=$(invoke "forge.data.write" '{"sql":"CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, email TEXT NOT NULL)"}')
echo "  Create: $(decode "$RESP")"
RESP=$(invoke "forge.data.write" "{\"sql\":\"INSERT INTO users (name, email) VALUES ('Alice', 'alice@example.com')\"}")
echo "  Insert: $(decode "$RESP")"
echo "  PASS"

echo ""
echo "=== Test 3b: data-sqlite query ==="
RESP=$(invoke "forge.data.query" '{"sql":"SELECT id, name, email FROM users"}')
DEC=$(decode "$RESP")
echo "  -> $DEC"
echo "$DEC" | grep -q "Alice" || { echo "FAIL"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 4: http-router full chain (router->auth->data) ==="
RESP=$(invoke "forge.http.route" '{"method":"GET","path":"/users","headers":{"authorization":"Bearer forge-demo-secret"},"body":""}')
DEC=$(decode "$RESP")
echo "  -> $DEC"
echo "$DEC" | grep -q '"status".*200' || { echo "FAIL (not 200)"; exit 1; }
echo "$DEC" | grep -q "Alice" || { echo "FAIL (no Alice)"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 5: http-router bad auth -> 401 ==="
RESP=$(invoke "forge.http.route" '{"method":"GET","path":"/users","headers":{"authorization":"Bearer wrong-token"},"body":""}')
DEC=$(decode "$RESP")
echo "  -> $DEC"
echo "$DEC" | grep -q '401' || { echo "FAIL (expected 401)"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 6: http-router POST /users ==="
RESP=$(invoke "forge.http.route" '{"method":"POST","path":"/users","headers":{"authorization":"Bearer forge-demo-secret"},"body":"{\"name\":\"Bob\",\"email\":\"bob@example.com\"}"}')
DEC=$(decode "$RESP")
echo "  -> $DEC"
echo "$DEC" | grep -q '"status".*200' || { echo "FAIL (not 200)"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 7: http-router 404 ==="
RESP=$(invoke "forge.http.route" '{"method":"DELETE","path":"/nonexistent","headers":{},"body":""}')
DEC=$(decode "$RESP")
echo "  -> $DEC"
echo "$DEC" | grep -q '404' || { echo "FAIL (expected 404)"; exit 1; }
echo "  PASS"

echo ""
echo "=== Test 8: Verify Bob was inserted via router POST ==="
RESP=$(invoke "forge.http.route" '{"method":"GET","path":"/users","headers":{"authorization":"Bearer forge-demo-secret"},"body":""}')
DEC=$(decode "$RESP")
echo "  -> $DEC"
echo "$DEC" | grep -q "Bob" || { echo "FAIL (Bob not found)"; exit 1; }
echo "  PASS"

echo ""
echo "=== ALL 8 TESTS PASSED ==="
echo "Summary: echo-rs | auth-jwt | data-sqlite | http-router chain | 401 | POST | 404 | persistence"
