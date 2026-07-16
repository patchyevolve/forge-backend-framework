#!/usr/bin/env bash
# Validates docs/11-BUILDING-A-SYSTEM.md end-to-end.
#
# Phase 1 — three plugins reach Ready without kernel restart
# Phase 2 — full pipeline: write → ingest → process → store
# Phase 3a — count plugin added at runtime via watch=true, with retry path
# Phase 3b — exhaustion path: a manifest whose process never appears lands in Stopped
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
TUTORIAL="$ROOT/examples/tutorial"
RELEASE="$ROOT/target/release"
TMPDIR="$(mktemp -d)"

FORGE_PID=""; STORE_PID=""; INGEST_PID=""; PROCESS_PID=""; COUNT_PID=""
cleanup() {
    echo "=== Cleaning up ==="
    for pid in "$FORGE_PID" "$COUNT_PID" "$PROCESS_PID" "$INGEST_PID" "$STORE_PID"; do
        kill "$pid" 2>/dev/null || true
    done
    rm -rf "$TMPDIR"
    # Clean up manifests that tests create at runtime (not part of startup)
    rm -f "$TUTORIAL/plugins/count/plugin.forge.toml"
    rmdir "$TUTORIAL/plugins/count" 2>/dev/null || true
    rm -rf "$TUTORIAL/plugins/ghost"
}
trap cleanup EXIT

# --- Helpers ---
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
status_has_state() {
    local name="$1" want="$2"
    curl -sf http://127.0.0.1:9091/v1/status | python3 -c "
import sys,json
d=json.load(sys.stdin)
for p in d['plugins']:
    if p['name'] == '$name' and p['state'] == '$want':
        sys.exit(0)
sys.exit(1)
" 2>/dev/null
}
status_has_capability() {
    local name="$1"
    curl -sf http://127.0.0.1:9091/v1/status | python3 -c "
import sys,json
d=json.load(sys.stdin)
for c in d['capabilities']:
    if c['name'] == '$name':
        sys.exit(0)
sys.exit(1)
" 2>/dev/null
}
wait_for_state() {
    local name="$1" want="$2" max_secs="${3:-10}"
    for i in $(seq 1 "$max_secs"); do
        if status_has_state "$name" "$want"; then
            return 0
        fi
        sleep 1
    done
    return 1
}

# Use a tutorial config with watch=true so Phase 3 works without kernel restart.
FORGE_CONFIG="$TMPDIR/forge.toml"
cat > "$FORGE_CONFIG" <<TOMLEOF
forge_config_version = "1.0"

[gateway]
grpc_bind = "127.0.0.1:9090"
http_bind = "127.0.0.1:9091"

[log]
level = "info"

[plugins]
manifest_dir = "$TUTORIAL/plugins"
watch = true
TOMLEOF

# Redirect forge logs to a file so we can grep the exhaustion terminal message.
FORGE_LOG="$TMPDIR/forge.log"

# ──────────────────────────────────────────────
echo "=== PHASE 1: three plugins reach Ready ==="

FORGE_LISTEN_ADDR="127.0.0.1:51052" "$RELEASE/tutorial-store" & STORE_PID=$!
FORGE_LISTEN_ADDR="127.0.0.1:51051" "$RELEASE/tutorial-ingest" & INGEST_PID=$!
FORGE_LISTEN_ADDR="127.0.0.1:51053" "$RELEASE/tutorial-process" & PROCESS_PID=$!
sleep 2

"$RELEASE/forge" run --config "$FORGE_CONFIG" > "$FORGE_LOG" 2>&1 & FORGE_PID=$!
echo "  forge PID $FORGE_PID"
sleep 3

wait_for_state "store" "Ready" 10 || { echo "FAIL: store never Ready"; exit 1; }
echo "  store -> Ready  PASS"
wait_for_state "ingest" "Ready" 10 || { echo "FAIL: ingest never Ready"; exit 1; }
echo "  ingest -> Ready  PASS"
wait_for_state "process" "Ready" 10 || { echo "FAIL: process never Ready"; exit 1; }
echo "  process -> Ready  PASS"

# ──────────────────────────────────────────────
echo ""
echo "=== PHASE 2: full pipeline ==="

DEC=$(decode "$(invoke "forge.example.ingest.write" '{"key":"my-key","value":"hello world"}')")
echo "  ingest.write -> $DEC"
echo "$DEC" | grep -q '"stored":true' || { echo "FAIL"; exit 1; }
echo "  PASS"

DEC=$(decode "$(invoke "forge.example.process" '{"key":"my-key","transform":"uppercase"}')")
echo "  process -> $DEC"
echo "$DEC" | grep -q '"original":"hello world"' || { echo "FAIL: missing original"; exit 1; }
echo "$DEC" | grep -q '"transformed":"HELLO WORLD"' || { echo "FAIL: missing transformed"; exit 1; }
echo "$DEC" | grep -q '"stored":true' || { echo "FAIL: missing stored"; exit 1; }
echo "  PASS"

DEC=$(decode "$(invoke "forge.example.process" '{"key":"my-key","transform":"reverse"}')")
echo "  process reverse -> $DEC"
echo "$DEC" | grep -q '"transformed":"dlrow olleh"' || { echo "FAIL: wrong reverse"; exit 1; }
echo "  PASS"

# ──────────────────────────────────────────────
echo ""
echo "=== PHASE 3a: count plugin runtime addition (watch=true retry path) ==="
echo "  Creating count manifest before process is running ..."
# The manifest is created first (simulates a user writing the config before
# starting the plugin process). The watcher discovers it, calls start_one_impl,
# fails with connection-refused, and leaves the plugin in Connecting. When the
# watcher retries on the next poll, start_one_impl succeeds.
COUNT_MANIFEST="$TUTORIAL/plugins/count/plugin.forge.toml"
mkdir -p "$(dirname "$COUNT_MANIFEST")"
cat > "$COUNT_MANIFEST" <<'TOMLEOF'
forge_manifest_version = "1.0"

[plugin]
name = "count"
version = "1.0.0"
description = "Counts invocations"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:51054"

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["forge.example.count@1.0"]
requires = []
TOMLEOF

# Wait for the watcher's first poll (3s) so it discovers the manifest and fails
sleep 4

# Now start the count process — the watcher's next poll will retry and connect
FORGE_LISTEN_ADDR="127.0.0.1:51054" "$RELEASE/tutorial-count" & COUNT_PID=$!
echo "  count PID $COUNT_PID"

# Wait up to 10s for watcher retry (next 3s poll + connect) to succeed
wait_for_state "count" "Ready" 10 || { echo "FAIL: count never Ready"; exit 1; }
echo "  count -> Ready  PASS"

# Verify the count capability is registered and callable
status_has_capability "forge.example.count" || { echo "FAIL: forge.example.count not in registry"; exit 1; }
echo "  forge.example.count registered  PASS"

DEC=$(decode "$(invoke "forge.example.count" "")")
echo "  count invoke #1 -> $DEC"
[ "$DEC" = "invocation #1" ] || { echo "FAIL: expected invocation #1"; exit 1; }
echo "  PASS"

DEC=$(decode "$(invoke "forge.example.count" "")")
echo "  count invoke #2 -> $DEC"
[ "$DEC" = "invocation #2" ] || { echo "FAIL: expected invocation #2"; exit 1; }
echo "  PASS"

# ──────────────────────────────────────────────
echo ""
echo "=== PHASE 3b: exhaustion path (plugin never starts) ==="
echo "  Creating ghost manifest at unreachable address ..."
# This manifest points to a port where nothing listens. The watcher retries
# with its own capped counter (watch_restart_attempts). After exhausting
# restart_max_attempts (3 for this test), the plugin lands in Stopped with
# a terminal log message. We use a low cap so the test finishes quickly.
GHOST_DIR="$TUTORIAL/plugins/ghost"
GHOST_MANIFEST="$GHOST_DIR/plugin.forge.toml"
mkdir -p "$GHOST_DIR"
cat > "$GHOST_MANIFEST" <<'TOMLEOF'
forge_manifest_version = "1.0"

[plugin]
name = "ghost"
version = "1.0.0"
description = "Never-starting plugin for exhaustion test"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:51999"

[lifecycle]
restart_policy = "on-failure"
restart_max_attempts = 3

[capabilities]
provides = []
requires = []
TOMLEOF

# Wait for the watcher to exhaust the cap and land in Stopped.
# 3 attempts x 3s poll = 9s minimum, plus 1-2s margin. Use 25s as safe upper bound.
if wait_for_state "ghost" "Stopped" 25; then
    echo "  ghost -> Stopped  PASS"
else
    echo "FAIL: ghost never reached Stopped (might still be retrying)"
    echo "  Current status:"
    curl -sf http://127.0.0.1:9091/v1/status | python3 -m json.tool 2>/dev/null || true
    exit 1
fi

# Verify the terminal log message appears in forge's log output
if grep -q "exhausted.*watch-retry.*giving up.*never reachable.*51999" "$FORGE_LOG"; then
    echo "  terminal log message  PASS"
else
    echo "FAIL: no terminal exhaustion message in forge log"
    echo "  Last 20 lines of forge log:"
    tail -20 "$FORGE_LOG"
    exit 1
fi

# Clean up the ghost manifest so it doesn't interfere
rm -rf "$GHOST_DIR"

echo ""
echo "=== ALL TUTORIAL TESTS PASSED ==="
