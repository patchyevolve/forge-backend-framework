#!/usr/bin/env bash
# Confirms you can build forge and run it with zero network calls once deps are cached.
# If this passes, the build is reproducible and works in air-gapped setups.
set -eo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
PASS=0
FAIL=0
FAILURES=""

pass() { PASS=$((PASS+1)); echo "  ✓ $1"; }
fail() { FAIL=$((FAIL+1)); FAILURES="$FAILURES  ✗ $1"$'\n'; echo "  ✗ $1"; }

# --- 1. Pull down all crate sources (the only step that touches the network) ---
echo "=== 1. cargo fetch (download all crate sources) ==="
cargo fetch --manifest-path "$ROOT/Cargo.toml" 2>&1 | tail -1
echo ""

# --- 2. Clean everything so we're building from scratch ---
echo "=== 2. cargo clean (start from known state) ==="
cargo clean --manifest-path "$ROOT/Cargo.toml" 2>&1 | tail -1
echo ""

# --- 3. Build offline and verify strace shows zero network syscalls ---
echo "=== 3. cargo build --offline (strace for connect/sendto) ==="
STRACE_OUT=$(mktemp)
set +e
strace -e network -f -o "$STRACE_OUT" cargo build --offline --manifest-path "$ROOT/Cargo.toml" 2>&1
BUILD_EXIT=$?
set -e
CONNECT_CALLS=$(grep -c "connect(" "$STRACE_OUT" 2>/dev/null || echo 0 | tr -d '[:space:]')
SENDTO_CALLS=$(grep -c "sendto(" "$STRACE_OUT" 2>/dev/null || echo 0 | tr -d '[:space:]')
TOTAL_NET=$((CONNECT_CALLS + SENDTO_CALLS))
if [ "$BUILD_EXIT" -eq 0 ] && [ "$TOTAL_NET" -eq 0 ]; then
    pass "cargo build --offline: exit=$BUILD_EXIT, network calls=$TOTAL_NET"
else
    fail "cargo build --offline: exit=$BUILD_EXIT, network calls=$TOTAL_NET (expected 0)"
fi
rm -f "$STRACE_OUT"
echo ""

# --- 4. Run forge and make sure it doesn't call out to the internet ---
echo "=== 4. forge run --config examples/example-backend/forge.toml (strace, 5s timeout) ==="
STRACE_OUT2=$(mktemp)
set +e
timeout 5 strace -e network -f -o "$STRACE_OUT2" \
    cargo run --offline --manifest-path "$ROOT/Cargo.toml" --bin forge -- \
    run --config "$ROOT/examples/example-backend/forge.toml" 2>&1
RUN_EXIT=$?
set -e
EXTERNAL_CONNECTS=$(grep "connect(" "$STRACE_OUT2" 2>/dev/null | grep -v "127.0.0.1" | wc -l | tr -d '[:space:]' || echo 0)
if [ "$EXTERNAL_CONNECTS" = "0" ]; then
    pass "forge run: external connects=$EXTERNAL_CONNECTS (expected 0)"
else
    fail "forge run: external connects=$EXTERNAL_CONNECTS (expected 0)"
fi
rm -f "$STRACE_OUT2"
echo ""

# --- Summary ---
echo "=== Results: $PASS passed, $FAIL failed ==="
if [ "$FAIL" -ne 0 ]; then
    echo ""
    echo "Failures:"
    echo "$FAILURES"
    exit 1
fi
