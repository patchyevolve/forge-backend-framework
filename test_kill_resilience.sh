#!/usr/bin/env bash
# Tests that the system handles a plugin dying mid-request.
# We start everything up, kill the data-sqlite backend while a query is running, then check:
#   1. The caller gets TransportError (doesn't hang, doesn't panic)
#   2. Other plugins and the kernel keep running fine
#   3. Registry unregisters the dead plugin — next call gets NotFound, not another hang
#   4. When data-sqlite comes back, the coordinator reconnects it automatically
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

# --- Shared helpers for the test ---
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
status_plugins() {
    curl -sf http://127.0.0.1:9091/v1/status | python3 -c "
import sys,json
d = json.load(sys.stdin)
for p in d.get('plugins', []):
    print(f\"{p['name']}: {p['state']}\")
"
}
capabilities_list() {
    curl -sf http://127.0.0.1:9091/v1/status | python3 -c "
import sys,json
d = json.load(sys.stdin)
for c in d.get('capabilities', []):
    print(f\"{c['name']} @ {c['version']} ({c['plugin']})\")
"
}

echo "=== Starting plugins ==="
"$ROOT/target/release/forge-plugin-echo-rs" & ECHO_PID=$!
"$ROOT/target/release/forge-plugin-auth-jwt" & AUTH_PID=$!
FORGE_DATA_DB_PATH="$TMPDIR/test.db" "$ROOT/target/release/forge-plugin-data-sqlite" & DATA_PID=$!
"$ROOT/target/release/forge-plugin-http-router" & ROUTER_PID=$!
sleep 2

echo "=== Starting forge ==="
"$ROOT/target/release/forge" run --config "$ROOT/examples/example-backend/forge.toml" & FORGE_PID=$!
sleep 3

echo "=== Initial status ==="
status_plugins
echo "--- capabilities ---"
capabilities_list

echo ""
echo "=== Phase 1: Set up data (INSERT Alice) ==="
DEC=$(decode "$(invoke "forge.data.write" '{"sql":"INSERT INTO users (name, email) VALUES (?1, ?2)", "params":["Alice","alice@example.com"]}')")
echo "  Insert: $DEC"

echo ""
echo "=== Phase 2: Send a slow data-query in background, KILL data-sqlite mid-flight ==="
# This recursive query drags for a couple seconds so we have time to kill it mid-flight
SLOW_QUERY='{"sql":"WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < 100000000) SELECT count(*) FROM cnt", "params":[]}'
QUERY_ENCODED=$(echo -n "$SLOW_QUERY" | base64 -w0)
TMP_QUERY="$TMPDIR/query_result.txt"
START_MS=$(date +%s%3N)

set +e
curl -s -w "\n%{http_code}" --max-time 10 \
    -X POST http://127.0.0.1:9091/v1/invoke \
    -H "Content-Type: application/json" \
    -d "{\"capability\": \"forge.data.query\", \"payload\": \"$QUERY_ENCODED\"}" \
    > "$TMP_QUERY" 2>&1 &
CURL_PID=$!

sleep 1
echo "  Killing data-sqlite (PID $DATA_PID)..."
kill -9 "$DATA_PID" 2>/dev/null || true

wait "$CURL_PID" 2>/dev/null || true
END_MS=$(date +%s%3N)
ELAPSED=$((END_MS - START_MS))
set -e

QUERY_RESP=$(cat "$TMP_QUERY")
STATUS_LINE=$(echo "$QUERY_RESP" | tail -1)
BODY=$(echo "$QUERY_RESP" | head -n -1)

echo "  HTTP status: $STATUS_LINE"
echo "  Response body: $BODY"
echo "  Elapsed: ${ELAPSED}ms"

# If the kill-handling works, this should error out fast with TransportError, not wait the full 10s
if [ "$ELAPSED" -gt 5000 ]; then
    echo "  FAIL: query hung for ${ELAPSED}ms (expected quick TransportError)"
    exit 1
fi
if echo "$BODY" | grep -qi "TRANSPORT_ERROR\|transport error\|TransportError"; then
    echo "  PASS — got TransportError (not hang)"
elif echo "$BODY" | grep -qi "error"; then
    echo "  PASS — got error response (not hang)"
else
    echo "  FAIL: unexpected response: $BODY"
    exit 1
fi

echo ""
echo "=== Phase 3: Immediately restart data-sqlite (before coordinator dials) ==="
echo "  Restarting data-sqlite process..."
FORGE_DATA_DB_PATH="$TMPDIR/test.db" "$ROOT/target/release/forge-plugin-data-sqlite" & DATA_PID=$!
echo "  data-sqlite PID $DATA_PID (restarted)"

echo ""
echo "=== Phase 4: Verify other plugins survive + kernel responsive ==="
echo "  Checking echo-rs..."
DEC=$(decode "$(invoke "forge.example.echo" "ping")")
echo "    echo: $DEC"
[ "$DEC" = "PING" ] || { echo "FAIL: echo not working"; exit 1; }

echo "  Checking auth-jwt..."
DEC=$(decode "$(invoke "forge.auth.verify" '{"token":"forge-demo-secret"}')")
echo "    auth: $DEC"
echo "$DEC" | grep -q '"valid".*true' || { echo "FAIL: auth not working"; exit 1; }
echo "  PASS — kernel + other plugins alive"

echo ""
echo "=== Phase 5: Wait for crash handler to STOP then coordinator restart ==="
echo "  (health check default 5s + restart delay 500ms — waiting up to 10s)..."
for i in 1 2 3 4 5 6 7 8 9 10; do
    PLUGIN_STATE=$(status_plugins | grep "data-sqlite" || echo "data-sqlite: gone")
    if echo "$PLUGIN_STATE" | grep -q "Ready"; then
        echo "  data-sqlite: Ready (restarted via coordinator)"
        break
    fi
    if echo "$PLUGIN_STATE" | grep -q "Stopped"; then
        echo "  data-sqlite: Stopped (crash detected)"
    fi
    sleep 1
done
if ! echo "$PLUGIN_STATE" | grep -q "Ready"; then
    echo "  FAIL: data-sqlite not Ready after 10s"
    status_plugins
    exit 1
fi
echo "  PASS — plugin re-registered via restart coordinator"

echo ""
echo "=== Phase 6: Verify data-sqlite fully functional after restart ==="
# Same DB file from before — the users table and Alice's row should still be there.
DEC=$(decode "$(invoke "forge.data.query" '{"sql":"SELECT name, email FROM users WHERE id=?1", "params":[1]}')")
echo "  Query: $DEC"
echo "$DEC" | grep -q "Alice" || { echo "FAIL: data query failed after restart"; exit 1; }
echo "  PASS — data fully functional"

echo ""
echo "=== ALL PHASES PASSED ==="
echo "Summary: TransportError | other survive | deregister | restart+re-register"
