# Building a Multi-Plugin System From Scratch

This tutorial walks through one complete worked example: a three-plugin data pipeline, built in dependency order, step by step. You'll end with a running system you can invoke through the HTTP gateway, and you'll see how the architecture's design choices pay off when you add, remove, or swap plugins later.

**Prerequisites:** You have `forge` installed (`forge --version` prints `1.0.0`), and you have a Rust toolchain with `cargo`. You'll also need `grpcurl` for standalone plugin testing — install it with `cargo install grpcurl` or `brew install grpcurl`.

---

## 1. The Scenario

We're building a simple data pipeline:

```
             ┌──────────┐
             │  ingest  │    stores raw data, returns it on query
             └────┬─────┘
                  │ forge.example.ingest.query
                  ▼
             ┌──────────┐
             │ process  │    reads from ingest, transforms, writes to store
             └────┬─────┘
                  │ forge.example.store.put
                  ▼
             ┌──────────┐
             │  store   │    persists processed data
             └──────────┘
```

Three plugins, two capability relationships:

| Plugin | Provides | Requires | Why |
|--------|----------|----------|-----|
| `store` | `forge.example.store.put` | *(none)* | No dependencies — write this first |
| `ingest` | `forge.example.ingest.write`, `forge.example.ingest.query` | *(none)* | No dependencies — write this second |
| `process` | `forge.example.process` | `forge.example.ingest.query`, `forge.example.store.put` | Depends on both above — write this last |

The process plugin uses `KernelClient` (Plugin SDK reference, §KernelClient) to call ingest's query capability, transform the data, then call store's put capability through the kernel — all in one invocation.

---

## 2. Deciding What Your Plugins Even Are

Before writing any code, you need to split your idea into capabilities and plugins. The Plugin Developer's Guide (§5, "The Routing-Plugin Pattern") describes two approaches:

- **Approach 1 — Coarse capability, internal sub-dispatch.** One plugin, one capability name, all routing logic inside.
- **Approach 2 — Fine-grained capabilities per route/operation.** Separate capability names per operation, possibly in separate plugins.

Our three-plugin design uses Approach 2: each operation gets its own capability name (`ingest.write`, `ingest.query`, `store.put`, `process`), and operations that logically belong together share a plugin (ingest handles both write and query).

How to decide what goes where:

1. **Start with operations, not processes.** List every operation your system performs: "store a value," "query a value," "transform data," "persist result."
2. **Group by change cadence.** Operations that change together and are owned by the same team should share a plugin. Operations that evolve independently should be in separate plugins.
3. **Look for dependency direction.** If A must call B, they should probably be separate plugins — the architecture is designed for this. If A and B always call each other, consider whether they're really one plugin.
4. **When in doubt, split.** Merging two plugins into one is trivial (copy the code into one binary). Splitting one plugin into two requires defining a capability boundary, which is harder to retrofit.

In our case: store and ingest are separate because they evolve independently (you might swap the storage backend without touching the ingest format), and process is separate because it orchestrates the other two.

---

## 3. Directory Layout

Create the project structure:

```
mkdir -p tutorial/plugins
cd tutorial
```

You'll have:

```
tutorial/
├── Cargo.toml               # workspace root (shared settings)
├── forge.toml               # kernel config
├── start.sh                 # startup script
└── plugins/                 # scanned by the kernel
    ├── store/
    │   ├── Cargo.toml
    │   ├── plugin.forge.toml
    │   └── src/main.rs
    ├── ingest/
    │   ├── Cargo.toml
    │   ├── plugin.forge.toml
    │   └── src/main.rs
    └── process/
        ├── Cargo.toml
        ├── plugin.forge.toml
        └── src/main.rs
```

Each plugin is its own Rust crate (Shape A — server pattern). You could also write any of them in Python, Go, or any language with gRPC support — the build order and workflow are the same; only the SDK calls differ.

### Workspace Root

Create `tutorial/Cargo.toml` so you can build all plugins with one command:

```toml
[workspace]
resolver = "2"
members = [
    "plugins/store",
    "plugins/ingest",
    "plugins/process",
]

[workspace.package]
version = "1.0.0"
edition = "2021"
license = "MIT"

[workspace.dependencies]
forge-plugin-sdk-rust = "1.0"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
anyhow = "1"
```

---

## 4. Plugin A — `store` (No Dependencies)

Write this one first because it has zero dependencies — no other plugin needs to be running to test it.

### plugins/store/Cargo.toml

```toml
[package]
name = "tutorial-store"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
forge-plugin-sdk-rust.workspace = true
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
```

### src/main.rs

```rust
use std::collections::HashMap;
use std::sync::Mutex;

use forge_plugin_sdk_rust::{
    Capability, InvokeContext, InvokeResult, Plugin, PluginError, PluginServer,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct PutRequest { key: String, value: String }

#[derive(Serialize)]
struct PutResponse { stored: bool, total: usize }

struct StorePlugin {
    records: Mutex<Vec<(String, String)>>,
}

#[forge_plugin_sdk_rust::async_trait]
impl Plugin for StorePlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("forge.example.store.put", "1.0.0")]
    }

    async fn health_check(&self) -> bool { true }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.store.put" => {
                let req: PutRequest = serde_json::from_slice(&ctx.payload)
                    .map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected PutRequest JSON: {e}"),
                        details: HashMap::new(),
                    })?;
                let mut records = self.records.lock().unwrap();
                records.push((req.key, req.value));
                Ok(serde_json::to_vec(&PutResponse {
                    stored: true,
                    total: records.len(),
                }).unwrap())
            }
            other => Err(PluginError::not_found(
                format!("unknown capability: {other}")
            )),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into()))
        .init();
    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:51052");
    }
    PluginServer::new(StorePlugin { records: Mutex::new(Vec::new()) })
        .serve_shape_a().await
}
```

Key details:
- The `Capability::new("forge.example.store.put", "1.0.0")` call declares what this plugin can do. The name is an opaque string — by convention it's `<domain>.<area>.<verb>` but the kernel does not enforce or interpret the namespace.
- The `FORGE_LISTEN_ADDR` default (51052) matches the address in `plugin.forge.toml`.
- `serve_shape_a()` starts a gRPC server that implements the `ForgePlugin` service (Register, Invoke, HealthCheck, Drain) — see Plugin Protocol Spec §5.

### plugin.forge.toml

```toml
forge_manifest_version = "1.0"

[plugin]
name = "store"
version = "1.0.0"
description = "Persists processed data"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:51052"

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["forge.example.store.put@1.0"]
requires = []
```

The `requires = []` is explicit: this plugin depends on nothing. The kernel uses the `provides` list during the live `Register` handshake (Plugin Protocol Spec §4), not from the manifest — the manifest's `provides`/`requires` is advisory metadata for operator tooling, explained in §7 below.

### Verify it standalone

Build and start the store plugin, then test it with `grpcurl` before Forge is even involved:

```bash
cargo build --release -p tutorial-store
FORGE_LISTEN_ADDR="127.0.0.1:51052" ./target/release/tutorial-store &
```

In another terminal:

```bash
grpcurl -plaintext -d '{"key":"test-key","value":"test-value"}' \
  127.0.0.1:51052 forge.plugin.v1.ForgePlugin/Invoke
```

Expected output:

```json
{
  "requestId": "",
  "result": {
    "payload": "eyJzdG9yZWQiOnRydWUsInRvdGFsIjoxfQ=="
  }
}
```

That base64 payload decodes to `{"stored":true,"total":1}` — the plugin works alone, no kernel needed. This is the core testing strategy: because the protocol is just gRPC, you can test any plugin with any gRPC client without running Forge at all. Kill the store plugin (`kill %1`) when you're done.

---

## 5. Plugin B — `ingest` (No Dependencies)

Same pattern: no dependencies, testable standalone.

### plugins/ingest/Cargo.toml

Same as store's, but `name = "tutorial-ingest"` instead of `"tutorial-store"`.

### src/main.rs

```rust
use std::collections::HashMap;
use std::sync::Mutex;

use forge_plugin_sdk_rust::{
    Capability, InvokeContext, InvokeResult, Plugin, PluginError, PluginServer,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct WriteRequest { key: String, value: String }

#[derive(Serialize)]
struct WriteResponse { stored: bool }

#[derive(Deserialize)]
struct QueryRequest { key: String }

#[derive(Serialize)]
struct QueryResponse { value: Option<String> }

struct IngestPlugin {
    data: Mutex<HashMap<String, String>>,
}

#[forge_plugin_sdk_rust::async_trait]
impl Plugin for IngestPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::new("forge.example.ingest.write", "1.0.0"),
            Capability::new("forge.example.ingest.query", "1.0.0"),
        ]
    }

    async fn health_check(&self) -> bool { true }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.ingest.write" => {
                let req: WriteRequest = serde_json::from_slice(&ctx.payload)
                    .map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected WriteRequest JSON: {e}"),
                        details: HashMap::new(),
                    })?;
                self.data.lock().unwrap().insert(req.key, req.value);
                Ok(serde_json::to_vec(&WriteResponse { stored: true }).unwrap())
            }
            "forge.example.ingest.query" => {
                let req: QueryRequest = serde_json::from_slice(&ctx.payload)
                    .map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected QueryRequest JSON: {e}"),
                        details: HashMap::new(),
                    })?;
                let value = self.data.lock().unwrap().get(&req.key).cloned();
                Ok(serde_json::to_vec(&QueryResponse { value }).unwrap())
            }
            other => Err(PluginError::not_found(
                format!("unknown capability: {other}")
            )),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into()))
        .init();
    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:51051");
    }
    PluginServer::new(IngestPlugin { data: Mutex::new(HashMap::new()) })
        .serve_shape_a().await
}
```

Two capabilities, one plugin. The `match ctx.capability.as_str()` dispatch is the "coarse capability, internal sub-dispatch" pattern (Plugin Developer's Guide §5, Approach 1) — a single `invoke` handler routes to the right logic based on which capability name was called.

### plugin.forge.toml

```toml
forge_manifest_version = "1.0"

[plugin]
name = "ingest"
version = "1.0.0"
description = "Ingests and stores data in memory — the source dataset"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:51051"

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["forge.example.ingest.write@1.0", "forge.example.ingest.query@1.0"]
requires = []
```

### Verify it standalone

```bash
cargo build --release -p tutorial-ingest
FORGE_LISTEN_ADDR="127.0.0.1:51051" ./target/release/tutorial-ingest &
```

Write some data:

```bash
grpcurl -plaintext -d '{"capability":"forge.example.ingest.write","payload":"eyJrZXkiOiJteS1rZXkiLCJ2YWx1ZSI6ImhlbGxvIHdvcmxkIn0="}' \
  127.0.0.1:51051 forge.plugin.v1.ForgePlugin/Invoke
```

(The payload is `{"key":"my-key","value":"hello world"}` base64-encoded.)

Query it back:

```bash
grpcurl -plaintext -d '{"capability":"forge.example.ingest.query","payload":"eyJrZXkiOiJteS1rZXkifQ=="}' \
  127.0.0.1:51051 forge.plugin.v1.ForgePlugin/Invoke
```

Expected:

```json
{
  "requestId": "",
  "result": {
    "payload": "eyJ2YWx1ZSI6ImhlbGxvIHdvcmxkIn0="
  }
}
```

Decodes to `{"value":"hello world"}`. Kill the ingest plugin. Both store and ingest work in isolation — we can now wire them together.

---

## 6. Plugin C — `process` (Depends on Ingest + Store)

This plugin has dependencies: it calls `forge.example.ingest.query` and `forge.example.store.put` through the kernel. This means the kernel must be running, and both ingest and store must be registered, before process can serve a request.

### plugins/process/Cargo.toml

Same pattern, `name = "tutorial-process"` instead of `"tutorial-store"`.

### src/main.rs

```rust
use std::collections::HashMap;

use forge_plugin_sdk_rust::{
    Capability, InvokeContext, InvokeResult, KernelClient, Plugin, PluginError, PluginServer,
};
use serde::{Deserialize, Serialize};

const KERNEL_GRPC: &str = "http://127.0.0.1:9090";

#[derive(Deserialize)]
struct ProcessRequest { key: String, transform: String }

#[derive(Serialize)]
struct ProcessResponse {
    original: String,
    transformed: String,
    stored: bool,
}

struct ProcessPlugin;

#[forge_plugin_sdk_rust::async_trait]
impl Plugin for ProcessPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("forge.example.process", "1.0.0")]
    }

    async fn health_check(&self) -> bool { true }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.process" => {
                let req: ProcessRequest = serde_json::from_slice(&ctx.payload)
                    .map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected ProcessRequest JSON: {e}"),
                        details: HashMap::new(),
                    })?;

                let kernel = KernelClient::connect(KERNEL_GRPC).await
                    .map_err(|e| PluginError {
                        code: "KERNEL_CONNECT_FAILED".into(),
                        message: e.to_string(),
                        details: HashMap::new(),
                    })?;

                // Step 1: query the ingest plugin
                let raw = kernel.invoke(
                    "forge.example.ingest.query",
                    serde_json::json!({"key": req.key}).to_string().into_bytes(),
                    ctx.metadata.clone(),
                    &ctx.request_id,
                ).await?;

                let response: serde_json::Value = serde_json::from_slice(&raw)
                    .map_err(|e| PluginError {
                        code: "INVALID_INGEST_RESPONSE".into(),
                        message: e.to_string(),
                        details: HashMap::new(),
                    })?;
                let original = response["value"].as_str().unwrap_or("").to_string();

                // Step 2: apply transform
                let transformed = match req.transform.as_str() {
                    "uppercase" => original.to_uppercase(),
                    "reverse" => original.chars().rev().collect(),
                    _ => original.clone(),
                };

                // Step 3: store the result
                let store_result = kernel.invoke(
                    "forge.example.store.put",
                    serde_json::json!({"key": req.key, "value": transformed}).to_string().into_bytes(),
                    ctx.metadata.clone(),
                    &ctx.request_id,
                ).await?;

                let store_json: serde_json::Value =
                    serde_json::from_slice(&store_result).unwrap_or_default();
                let stored = store_json["stored"].as_bool().unwrap_or(false);

                Ok(serde_json::to_vec(&ProcessResponse {
                    original, transformed, stored,
                }).unwrap())
            }
            other => Err(PluginError::not_found(
                format!("unknown capability: {other}")
            )),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into()))
        .init();
    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:51053");
    }
    PluginServer::new(ProcessPlugin).serve_shape_a().await
}
```

This plugin uses `KernelClient` (Plugin SDK reference) to call other capabilities through the kernel's gRPC gateway. The address `http://127.0.0.1:9090` is the kernel's gRPC bind address (the default from `forge.toml`). The process plugin itself is just another server — the kernel connects to it, and it connects back to the kernel to call downstream capabilities.

### plugin.forge.toml

```toml
forge_manifest_version = "1.0"

[plugin]
name = "process"
version = "1.0.0"
description = "Transforms ingested data and stores the result"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:51053"

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["forge.example.process@1.0"]
requires = ["forge.example.ingest.query@1.0", "forge.example.store.put@1.0"]
```

Note `requires` lists the two capabilities this plugin depends on. What this means — and what it doesn't — is the subject of the next section.

---

## 7. Understanding `requires` (and What It Does NOT Do)

The `requires` field in `plugin.forge.toml` is **advisory metadata for operator tooling**. It is NOT:

- ❌ A startup-order directive — the kernel does NOT wait for required capabilities to appear before starting this plugin.
- ❌ A gate on registration — the kernel does NOT refuse to register a plugin whose required capabilities aren't present.
- ❌ A runtime check — the kernel does NOT enforce that required capabilities are available at invoke time. If you call a capability that no plugin provides, you get a `NotFound` error from `bus::dispatch`, not from `requires` validation.

What it IS:

- ✅ **Documentation** — tells operators what capabilities a plugin expects to use.
- ✅ **Visualization input** — `forge status --graph` reads `requires` across all discovered manifests and draws the dependency graph, without needing any plugin to be running.

This is explicitly stated in the Plugin Protocol Spec (§3, comments on `requires`):

> `requires = []` — capabilities this plugin expects to be able to CALL (informational + used by `forge status --graph`)
>
> *Why `provides`/`requires` exist in the manifest when the live handshake is authoritative: purely for operator ergonomics — `forge status --graph` can draw the capability dependency graph across all discovered plugins before any of them have actually connected.*

### See it for yourself

Before any plugins are running, run:

```bash
cargo build --release
forge status --config tutorial/forge.toml --graph
```

Output:

```
=== Capability Dependency Graph ===

  ingest
    provides:
      - forge.example.ingest.write@1.0
      - forge.example.ingest.query@1.0

  store
    provides:
      - forge.example.store.put@1.0

  process
    provides:
      - forge.example.process@1.0
    requires:
      - forge.example.ingest.query@1.0  → ingest
      - forge.example.store.put@1.0  → store
```

This graph renders from the TOML files alone — no gRPC connections, no running processes. You can see at a glance that process's dependencies are satisfied (ingest provides `ingest.query`, store provides `store.put`). If a dependency were missing, it would show as `(unresolved — no provider found)`.

### So how DO you handle actual runtime dependency order?

Since `requires` is advisory, the kernel treats all discovered plugins equally — it attempts to connect to every one at startup and will fail if a plugin isn't reachable. No plugin is retried automatically after a failed initial connection.

This means **all plugins must be running before the kernel starts**.

The process plugin uses `KernelClient` *inside its `invoke` handler*, not at startup, so it doesn't need the kernel to be up when it starts. Start everything before the kernel:

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"

# 1. Start every plugin (order doesn't matter — none connect to each other at startup)
FORGE_LISTEN_ADDR="127.0.0.1:51052" "$ROOT/target/release/tutorial-store" &
FORGE_LISTEN_ADDR="127.0.0.1:51051" "$ROOT/target/release/tutorial-ingest" &
FORGE_LISTEN_ADDR="127.0.0.1:51053" "$ROOT/target/release/tutorial-process" &
sleep 2

# 2. Start the kernel — it connects to all three at once
forge run --config "$ROOT/tutorial/forge.toml" &

wait
```

If you do need to start a plugin after the kernel (e.g. because its manifest wasn't available at startup), restart the kernel so it re-discovers and re-connects to all plugins.

**Option B — Retry/backoff in the dependent plugin (more robust).** If a plugin must start before the kernel, use a retry loop for `KernelClient::connect`:

```rust
async fn connect_with_retry() -> KernelClient {
    loop {
        if let Ok(client) = KernelClient::connect(KERNEL_GRPC).await {
            return client;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
```

For production systems, use Option B (or combine both). The SDK doesn't provide automatic retry — it's intentionally left to the plugin author so the behavior is explicit.

---

## 8. Writing the Kernel Config

Create `tutorial/forge.toml`:

```toml
forge_config_version = "1.0"

[gateway]
grpc_bind = "127.0.0.1:9090"
http_bind = "127.0.0.1:9091"

[log]
level = "info"

[plugins]
manifest_dir = "plugins"
```

The `manifest_dir` is relative to the config file's directory. When you run `forge run --config tutorial/forge.toml`, it resolves `plugins/` relative to `tutorial/`, so it finds `tutorial/plugins/store/plugin.forge.toml`, etc.

---

## 9. Running the Full System

### Build everything

```bash
cargo build --release
```

### Start the stack

The kernel connects to every discovered plugin at startup and never retries failed connections, so **all plugins must be running before the kernel starts**.

```bash
# Terminal 1: start every plugin (order doesn't matter)
FORGE_LISTEN_ADDR="127.0.0.1:51052" ./target/release/tutorial-store &
FORGE_LISTEN_ADDR="127.0.0.1:51051" ./target/release/tutorial-ingest &

# Terminal 2:
FORGE_LISTEN_ADDR="127.0.0.1:51053" ./target/release/tutorial-process &

# Terminal 3: start the kernel
sleep 2
forge run --config tutorial/forge.toml
```

You should see:

```
INFO forge: forge-cli 1.0.0 starting
INFO forge: config loaded from tutorial/forge.toml (forge_config_version 1.0 — OK)
INFO forge: plugin discovered: store (tutorial/plugins/store/plugin.forge.toml)
INFO forge: plugin discovered: ingest (tutorial/plugins/ingest/plugin.forge.toml)
INFO forge: plugin discovered: process (tutorial/plugins/process/plugin.forge.toml)
INFO forge_backend::lifecycle::manager: plugin store: READY — capabilities registered
INFO forge_backend::lifecycle::manager: plugin ingest: READY — capabilities registered
INFO forge_backend::lifecycle::manager: plugin process: READY — capabilities registered
INFO forge: forge-cli ready — accepting connections
INFO forge_gateway::grpc: gRPC gateway listening on 127.0.0.1:9090
INFO forge_gateway::http: HTTP gateway listening on 127.0.0.1:9091 (TLS: disabled — local dev only)
```

### Check status

```bash
curl http://127.0.0.1:9091/v1/status
```

```json
{
  "plugins": [
    { "name": "store", "state": "Ready" },
    { "name": "ingest", "state": "Ready" },
    { "name": "process", "state": "Ready" }
  ],
  "capabilities": [
    { "name": "forge.example.store.put", "version": "1.0.0", "plugin": "store" },
    { "name": "forge.example.ingest.write", "version": "1.0.0", "plugin": "ingest" },
    { "name": "forge.example.ingest.query", "version": "1.0.0", "plugin": "ingest" },
    { "name": "forge.example.process", "version": "1.0.0", "plugin": "process" }
  ]
}
```

### Run the full pipeline

Step 1 — Write data into ingest:

```bash
curl -X POST http://127.0.0.1:9091/v1/invoke \
  -H "Content-Type: application/json" \
  -d '{"capability":"forge.example.ingest.write","payload":"eyJrZXkiOiJteS1rZXkiLCJ2YWx1ZSI6ImhlbGxvIHdvcmxkIn0="}'
```

(The payload is `{"key":"my-key","value":"hello world"}` base64-encoded.)

Response:

```json
{"request_id":"...","payload":"eyJzdG9yZWQiOnRydWV9"}
```

Step 2 — Invoke the process pipeline:

```bash
curl -X POST http://127.0.0.1:9091/v1/invoke \
  -H "Content-Type: application/json" \
  -d '{"capability":"forge.example.process","payload":"eyJrZXkiOiJteS1rZXkiLCJ0cmFuc2Zvcm0iOiJ1cHBlcmNhc2UifQ=="}'
```

(The payload is `{"key":"my-key","transform":"uppercase"}` base64-encoded.)

Response:

```json
{
  "request_id": "...",
  "payload": "eyJvcmlnaW5hbCI6ImhlbGxvIHdvcmxkIiwidHJhbnNmb3JtZWQiOiJIRUxMTyBXT1JMRCIsInN0b3JlZCI6dHJ1ZX0="
}
```

Decode the payload:

```bash
echo 'eyJvcmlnaW5hbCI6ImhlbGxvIHdvcmxkIiwidHJhbnNmb3JtZWQiOiJIRUxMTyBXT1JMRCIsInN0b3JlZCI6dHJ1ZX0=' | base64 -d
```

```json
{"original":"hello world","transformed":"HELLO WORLD","stored":true}
```

The full pipeline works: ingest stored it, process queried it, uppercased it, and pushed the result to store. Try `transform=reverse` as well.

---

## 10. Adding a Fourth Plugin at Runtime

One of Forge's design goals is that you can add a plugin without restarting the kernel. The `watch = true` config option enables this.

### Set up the hot-reload config

Edit `tutorial/forge.toml` to add `watch = true`:

```toml
[plugins]
manifest_dir = "plugins"
watch = true
```

Stop and restart the kernel with this config.

### Create a fourth plugin

Let's add a `count` plugin that returns how many records are in the store. Create `tutorial/plugins/count/`:

**plugins/count/Cargo.toml:** same template, `name = "tutorial-count"` instead of `"tutorial-store"`.

Add the count plugin to the workspace in `tutorial/Cargo.toml`:

```toml
members = [
    "plugins/store",
    "plugins/ingest",
    "plugins/process",
    "plugins/count",
]
```

**plugins/count/src/main.rs:**

```rust
use std::sync::atomic::{AtomicUsize, Ordering};

use forge_plugin_sdk_rust::{
    Capability, InvokeContext, InvokeResult, Plugin, PluginServer,
};

struct CountPlugin {
    counter: AtomicUsize,
}

#[forge_plugin_sdk_rust::async_trait]
impl Plugin for CountPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("forge.example.count", "1.0.0")]
    }

    async fn health_check(&self) -> bool { true }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        let count = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(format!("invocation #{count}").into_bytes())
    }
}
```

**plugin.forge.toml:**

```toml
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
```

### Add it at runtime

Build the plugin and add it to the workspace:

```bash
cargo build --release -p tutorial-count
```

Stop the kernel with Ctrl+C, then start the count plugin and restart the kernel. Since the kernel reads all manifests from `plugins/` at startup, it will discover the count plugin:

```bash
FORGE_LISTEN_ADDR="127.0.0.1:51054" ./target/release/tutorial-count &
sleep 2
forge run --config tutorial/forge.toml
```

The kernel discovers and connects to all four plugins:

```

```
INFO forge: plugin discovered: count (tutorial/plugins/count/plugin.forge.toml)
INFO forge_backend::lifecycle::manager: plugin count: READY — capabilities registered
```

Verify:

```bash
curl http://127.0.0.1:9091/v1/status | python3 -m json.tool
```

The count plugin appears in the list. Invoke it:

```bash
curl -X POST http://127.0.0.1:9091/v1/invoke \
  -H "Content-Type: application/json" \
  -d '{"capability":"forge.example.count","payload":""}'
```

Each call increments the counter.

---

## 11. Removing or Swapping a Plugin

Because each plugin is an independent process that registers its capabilities with the kernel, removing one requires no changes to the others.

### Remove a plugin

Kill the count plugin's process:

```bash
kill %1  # or pkill tutorial-count
```

The kernel's health check detects the failure, marks it as `Stopped`, and the registry deregisters its capabilities. The next `forge.example.count` invocation returns:

```json
{"error":{"code":"NOT_FOUND","message":"no plugin registered for capability: forge.example.count"}}
```

No other plugin is affected — store, ingest, and process continue serving.

### Swap a plugin

Moving from the in-memory `store` plugin to a SQLite-backed one (like `data-sqlite`) means:
1. Write the new plugin (or use the existing `forge-plugin-data-sqlite`)
2. Keep the same capability name: `forge.example.store.put`
3. Start the new plugin, stop the old one
4. Nothing else changes — process still calls `forge.example.store.put` and gets routed to the new implementation

This is the concrete payoff of capability-based routing: **callers are decoupled from implementations by the capability name**. As long as the capability contract (input/output format) is preserved, the implementation can be replaced without touching any caller.

---

## 12. What You've Built

```
                        ┌─────────────────────────────┐
                        │     HTTP/gRPC Gateway        │
                        │   (port 9090 / 9091)          │
                        └──────────┬──────────────────┘
                                   │
                        ┌──────────▼──────────────────┐
                        │     Forge Kernel              │
                        │  Registry  │  Bus  │ Manager  │
                        └──────┬──────────┬───────────┘
                               │          │
          ┌────────────────────┼──────────┼──────────────┐
          │                    │          │              │
     ┌────▼─────┐       ┌─────▼─────┐  ┌─▼───────┐  ┌───▼────┐
     │  ingest  │       │  process  │  │  store  │  │ count  │
     │  :51051  │       │  :51053   │  │ :51052  │  │ :51054 │
     └──────────┘       └───────────┘  └─────────┘  └────────┘
```

- Plugins are independent processes that speak gRPC
- The kernel routes invocations by capability name, not by plugin identity
- Dependencies are declared in manifests for visualization, enforced at runtime by the calling plugin
- New plugins can be added without touching existing ones
- Any plugin can be swapped as long as the capability contract is preserved

---

## Appendix: Startup Script

Save this as `tutorial/start.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cleanup() {
    echo "=== Shutting down ==="
    for pid in "$FORGE_PID" "$PROCESS_PID" "$INGEST_PID" "$STORE_PID"; do
        kill "$pid" 2>/dev/null || true
    done
    wait
}
trap cleanup EXIT

echo "=== Starting all plugins ==="
FORGE_LISTEN_ADDR="127.0.0.1:51052" "$ROOT/target/release/tutorial-store" & STORE_PID=$!
FORGE_LISTEN_ADDR="127.0.0.1:51051" "$ROOT/target/release/tutorial-ingest" & INGEST_PID=$!
FORGE_LISTEN_ADDR="127.0.0.1:51053" "$ROOT/target/release/tutorial-process" & PROCESS_PID=$!
sleep 2

echo "=== Starting forge ==="
forge run --config "$ROOT/tutorial/forge.toml" & FORGE_PID=$!

wait
```
