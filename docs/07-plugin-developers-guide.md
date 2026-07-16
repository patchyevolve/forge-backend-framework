# Plugin Developer's Guide

A Forge plugin is a standalone Rust binary that implements the `Plugin` trait from the `forge` crate's `sdk` module. Forge spawns it as a managed subprocess, connects via gRPC, learns its capabilities, and routes invocations to it.

## 1. How It Works

```
Forge Kernel                    Plugin Process
     │                               │
     │  Spawns subprocess             │
     │  with FORGE_LISTEN_ADDR ───────┤
     │  and FORGE_PLUGIN_NAME         │
     │                               │
     │  ───── gRPC Connect ────────→  │
     │                               │
     │  ←── Register (capabilities) ─ │
     │                               │
     │  ←── HealthCheck (every 5s) ── │
     │                               │
     │  ── Invoke (handle request) ──→│
     │  ←── Response ─────────────────│
     │                               │
     │  ── Drain (shutdown signal) ──→│
     │                               │
```

Every plugin is a gRPC server that implements four RPCs:

| RPC | Direction | Purpose |
|---|---|---|
| `Register` | Plugin → Kernel | Advertise capabilities at startup |
| `Invoke` | Kernel → Plugin | Handle a capability invocation |
| `HealthCheck` | Kernel → Plugin | Are you alive? (periodic) |
| `Drain` | Kernel → Plugin | Graceful shutdown requested |

The `forge::sdk` module wraps these into a simple Rust trait so you never touch gRPC directly.

## 2. Quick Start: Your First Plugin

### 2.1 Create the plugin

```bash
cd your-project
forge new plugin my-capability
```

This creates `forge/plugins/my-capability/` with:

```
forge/plugins/my-capability/
├── Cargo.toml
├── src/
│   └── main.rs
```

### 2.2 Implement the Plugin trait

Edit `src/main.rs`:

```rust
use forge::sdk::{
    Capability, InvokeContext, InvokeResult, Plugin, PluginServer,
};
use forge::sdk::PluginError;

struct MyPlugin;

#[forge::sdk::async_trait]
impl Plugin for MyPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::new("app.my_capability", "1.0.0"),
        ]
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "app.my_capability" => {
                let input = String::from_utf8_lossy(&ctx.payload);
                let result = format!("hello from my plugin: {input}");
                Ok(result.into_bytes())
            }
            other => Err(PluginError::not_found(format!("unknown: {other}"))),
        }
    }

    async fn health_check(&self) -> bool { true }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into()))
        .init();
    PluginServer::new(MyPlugin).serve_shape_a().await
}
```

### 2.3 Add a route in forge.toml

Edit `forge/forge.toml` and add:

```toml
[[gateway.routes]]
method = "GET"
path = "/my-capability"
capability = "app.my_capability@1.0"
```

### 2.4 Build and run

```bash
cargo build --release
forge run
```

Then test:

```bash
curl http://localhost:9091/my-capability
```

## 3. Plugin SDK Reference

### 3.1 The Plugin Trait

```rust
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// REQUIRED: Advertise the capabilities this plugin provides.
    fn capabilities(&self) -> Vec<Capability>;

    /// REQUIRED: Handle an invocation for one of your capabilities.
    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult;

    /// REQUIRED: Return false to signal the kernel you're unhealthy.
    async fn health_check(&self) -> bool;

    /// OPTIONAL: Called before the kernel force-kills you.
    async fn on_drain(&self) {}
}
```

### 3.2 Capability

```rust
pub struct Capability {
    pub name: String,             // e.g. "app.my_action"
    pub version: String,          // e.g. "1.0.0"
    pub input_schema_ref: String, // JSON Schema URL (optional)
    pub output_schema_ref: String, // JSON Schema URL (optional)
}

impl Capability {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self;
}
```

### 3.3 InvokeContext

```rust
pub struct InvokeContext {
    pub request_id: String,                 // Unique ID for tracing
    pub capability: String,                 // The capability being invoked
    pub payload: Vec<u8>,                   // Opaque payload (typically JSON)
    pub metadata: HashMap<String, String>,  // Key-value metadata from caller
}
```

### 3.4 PluginError

```rust
pub struct PluginError {
    pub code: String,                     // Machine-readable code, e.g. "NOT_FOUND"
    pub message: String,                  // Human-readable description
    pub details: HashMap<String, String>, // Structured details
}

impl PluginError {
    pub fn not_found(message: impl Into<String>) -> Self;
}
```

### 3.5 PluginServer

```rust
pub struct PluginServer<P: Plugin> { .. }

impl<P: Plugin> PluginServer<P> {
    /// Wrap a plugin ready for serving.
    pub fn new(plugin: P) -> Self;

    /// Start serving. Reads FORGE_LISTEN_ADDR from env.
    pub async fn serve_shape_a(self) -> anyhow::Result<()>;
}
```

The `serve_shape_a` method:
1. Reads `FORGE_LISTEN_ADDR` env var (set by Forge when spawning)
2. Supports TCP (`127.0.0.1:50051`) or Unix sockets (`unix:///tmp/plugin.sock`)
3. Falls back to `unix:///tmp/forge-plugin.sock` if `FORGE_LISTEN_ADDR` is not set
4. Starts the gRPC server and blocks forever

### 3.6 KernelClient (calling other plugins)

A plugin can call other plugins through the Forge kernel:

```rust
pub struct KernelClient { .. }

impl KernelClient {
    /// Connect to the kernel's gRPC gateway.
    pub async fn connect(grpc_addr: &str) -> Result<Self, anyhow::Error>;

    /// Invoke a capability through the kernel.
    pub async fn invoke(
        &self,
        capability: &str,
        payload: Vec<u8>,
        metadata: HashMap<String, String>,
        request_id: &str,
    ) -> InvokeResult;
}
```

Example — a plugin that calls an auth capability:

```rust
async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
    // Connect to kernel's gRPC gateway
    let kernel = KernelClient::connect("http://127.0.0.1:9090").await
        .map_err(|e| PluginError {
            code: "KERNEL_CONNECT_FAILED".into(),
            message: e.to_string(),
            details: HashMap::new(),
        })?;

    // Call another capability
    let response = kernel.invoke(
        "app.auth.verify@1.0",
        serde_json::json!({"token": "..."}).to_string().into_bytes(),
        ctx.metadata.clone(),
        &ctx.request_id,
    ).await?;

    Ok(response)
}
```

## 4. Handling Multiple Capabilities

A plugin can register multiple capabilities. Dispatch in `invoke` by matching `ctx.capability`:

```rust
fn capabilities(&self) -> Vec<Capability> {
    vec![
        Capability::new("app.users.list", "1.0.0"),
        Capability::new("app.users.create", "1.0.0"),
    ]
}

async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
    match ctx.capability.as_str() {
        "app.users.list" => { /* list users */ }
        "app.users.create" => { /* create user */ }
        other => Err(PluginError::not_found(format!("unknown: {other}"))),
    }
}
```

## 5. Input/Output Conventions

### Receiving JSON

Plugins receive raw bytes in `ctx.payload`. When called through the HTTP gateway, JSON bodies are passed as-is:

```rust
let input: MyInput = serde_json::from_slice(&ctx.payload)
    .map_err(|e| PluginError {
        code: "INVALID_PAYLOAD".into(),
        message: e.to_string(),
        details: HashMap::new(),
    })?;
```

### Returning JSON

Return `Ok(bytes)` from `invoke`. The HTTP gateway wraps the bytes in a JSON envelope:

```rust
let output = serde_json::to_vec(&MyResponse { success: true }).unwrap();
Ok(output)
```

The gateway returns:

```json
{"payload":{"success":true}}
```

### Returning Errors

```rust
return Err(PluginError {
    code: "RATE_LIMITED".into(),
    message: "too many requests".into(),
    details: HashMap::new(),
});
```

The gateway returns:

```json
{"error":{"code":"RATE_LIMITED","message":"too many requests"}}
```

## 6. Testing Your Plugin

### With curl (through the running kernel)

```bash
curl http://localhost:9091/health
curl -X POST http://localhost:9091/calc/add \
  -H "Content-Type: application/json" \
  -d '{"a":10,"b":3}'
```

### Through the invoke endpoint

```bash
curl -X POST http://localhost:9091/v1/invoke \
  -H "Content-Type: application/json" \
  -d '{"capability":"app.my_capability","payload":"'$(echo -n '{"key":"value"}' | base64)'"}'
```

### Unit tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_invoke() {
        let plugin = MyPlugin;
        let ctx = InvokeContext {
            request_id: "test-1".into(),
            capability: "app.my_capability".into(),
            payload: b"test data".to_vec(),
            metadata: HashMap::new(),
        };
        let result = plugin.invoke(ctx).await.unwrap();
        assert!(String::from_utf8_lossy(&result).contains("test data"));
    }

    #[tokio::test]
    async fn test_health() {
        let plugin = MyPlugin;
        assert!(plugin.health_check().await);
    }
}
```

## 7. Stateful Plugins

Plugins can hold state. Use `Mutex`, `RwLock`, or `tokio::sync` types:

```rust
use std::sync::Mutex;

struct MyPlugin {
    counter: Mutex<u64>,
}

impl Plugin for MyPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("app.counter", "1.0.0")]
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        let mut count = self.counter.lock().unwrap();
        *count += 1;
        Ok(format!("invocation #{count}").into_bytes())
    }
}

// In main():
let plugin = MyPlugin { counter: Mutex::new(0) };
PluginServer::new(plugin).serve_shape_a().await
```

## 8. Health Checks

Return `true` if the plugin is healthy. Check your dependencies:

```rust
async fn health_check(&self) -> bool {
    // Check database connection
    sqlx::query("SELECT 1").fetch_one(&self.pool).await.is_ok()
}
```

If `health_check` returns `false` too many times (configurable threshold), Forge restarts the plugin.

## 9. Graceful Shutdown (Drain)

Override `on_drain` to clean up resources before Forge kills the process:

```rust
async fn on_drain(&self) {
    tracing::info!("shutting down gracefully...");
    self.db.close().await;
    self.queue.flush().await;
}
```

## 10. What Plugins Can't Do

- **Can't bind arbitrary ports** — Forge assigns the gRPC port. The plugin listens only on `FORGE_LISTEN_ADDR`.
- **Can't outlive the kernel** — Forge kills the subprocess on shutdown.
- **Can't bypass the gateway** — All external requests go through the HTTP/gRPC gateway.
- **Can't see other plugins directly** — Plugins communicate through the kernel only, via `KernelClient`.
