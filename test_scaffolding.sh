#!/usr/bin/env bash
set -euo pipefail

# Integration tests for CLI scaffolding: forge init, forge new plugin, forge install.

ROOT="$(cd "$(dirname "$0")" && pwd)"
FORGE_BIN="$ROOT/target/release/forge"

if [ ! -x "$FORGE_BIN" ]; then
    echo "Building forge-cli first..."
    cargo build -p forge-cli --release 2>&1
fi

cleanup() {
    echo "=== Cleaning up ==="
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

TMPDIR="$(mktemp -d)"

# ---- Test 1: forge init (bootstrap project) ----------------------------------
echo "=== Test 1: forge init ==="
cd "$TMPDIR"
"$FORGE_BIN" init my-api 2>&1
if [ -f "my-api/forge/forge.toml" ] && [ -d "my-api/forge/plugins" ]; then
    echo "PASS: Project created with forge/forge.toml and forge/plugins/"
else
    echo "FAIL: Project structure incomplete"
    find my-api -type f | head -20
    exit 1
fi

# Verify forge.toml has correct defaults
if grep -q "forge_config_version" "my-api/forge/forge.toml"; then
    echo "PASS: forge.toml has config version"
else
    echo "FAIL: forge.toml missing config version"
    exit 1
fi

# ---- Test 2: forge new plugin in project ------------------------------------
echo "=== Test 2: forge new plugin ==="
cd "$TMPDIR/my-api"
"$FORGE_BIN" new plugin my-handler 2>&1
if [ -f "forge/plugins/my-handler/Cargo.toml" ] && \
   [ -f "forge/plugins/my-handler/src/main.rs" ] && \
   [ -f "forge/plugins/my-handler/plugin.forge.toml" ]; then
    echo "PASS: Plugin created with all files"
else
    echo "FAIL: Plugin structure incomplete"
    ls -la forge/plugins/my-handler/ 2>/dev/null || echo "  (directory not found)"
    exit 1
fi

# Verify plugin files
if grep -q 'forge = ' "forge/plugins/my-handler/Cargo.toml"; then
    echo "PASS: Cargo.toml has forge dependency"
else
    echo "FAIL: Cargo.toml missing forge dependency"
    exit 1
fi

if grep -q "impl Plugin for MyPlugin" "forge/plugins/my-handler/src/main.rs"; then
    echo "PASS: main.rs has Plugin implementation"
else
    echo "FAIL: main.rs missing Plugin implementation"
    exit 1
fi

if grep -q 'provides = \["my:action@1.0"\]' "forge/plugins/my-handler/plugin.forge.toml"; then
    echo "PASS: plugin.forge.toml declares capabilities"
else
    echo "FAIL: plugin.forge.toml missing capabilities"
    exit 1
fi

# ---- Test 3: forge new plugin with custom dir --------------------------------
echo "=== Test 3: forge new plugin with custom dir ==="
cd "$TMPDIR"
mkdir -p custom-plugins
"$FORGE_BIN" new plugin custom-thing --dir custom-plugins 2>&1
if [ -f "custom-plugins/custom-thing/plugin.forge.toml" ]; then
    echo "PASS: Plugin created in custom directory"
else
    echo "FAIL: Plugin not in custom directory"
    exit 1
fi

# ---- Test 4: forge install known plugin --------------------------------------
echo "=== Test 4: forge install known plugin ==="
cd "$TMPDIR"
"$FORGE_BIN" install auth-jwt 2>&1
if [ -f "plugins/auth-jwt/plugin.forge.toml" ]; then
    echo "PASS: auth-jwt manifest created"
else
    echo "FAIL: auth-jwt manifest missing"
    exit 1
fi

if grep -q "forge.auth.verify" "plugins/auth-jwt/plugin.forge.toml"; then
    echo "PASS: auth-jwt has correct capability"
else
    echo "FAIL: auth-jwt capability mismatch"
    exit 1
fi

# ---- Test 5: forge install unknown plugin (creates stub) ---------------------
echo "=== Test 5: forge install unknown plugin ==="
cd "$TMPDIR"
"$FORGE_BIN" install my-custom-plugin --dir custom 2>&1
if [ -f "custom/plugin.forge.toml" ]; then
    echo "PASS: Unknown plugin stub created"
else
    echo "FAIL: Unknown plugin stub missing"
    exit 1
fi

if grep -q "my-custom-plugin" "custom/plugin.forge.toml"; then
    echo "PASS: Stub has correct name"
else
    echo "FAIL: Stub name mismatch"
    exit 1
fi

# ---- Test 6: forge install fails if already exists ---------------------------
echo "=== Test 6: forge install duplicate check ==="
cd "$TMPDIR"
RESULT=$("$FORGE_BIN" install auth-jwt 2>&1 || true)
if echo "$RESULT" | grep -qi "already exists"; then
    echo "PASS: Duplicate install rejected"
else
    echo "FAIL: Expected 'already exists' error"
    exit 1
fi

# ---- Test 7: forge init fails if already exists ------------------------------
echo "=== Test 7: forge init duplicate ==="
cd "$TMPDIR"
RESULT=$("$FORGE_BIN" init my-api 2>&1 || true)
if echo "$RESULT" | grep -qi "already exists"; then
    echo "PASS: Duplicate project rejected"
else
    echo "FAIL: Expected 'already exists' error"
    exit 1
fi

echo ""
echo "=== All scaffolding tests passed ==="
