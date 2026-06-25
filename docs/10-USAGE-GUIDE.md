# Forge Usage Guide

## Table of Contents

1. [What is Forge?](#what-is-forge)
2. [Installation](#installation)
3. [Directory Layout](#directory-layout)
4. [Kernel Configuration (forge.toml)](#kernel-configuration-forgetoml)
5. [Plugin Manifests (plugin.forge.toml)](#plugin-manifests-pluginforgetoml)
6. [Starting Forge](#starting-forge)
7. [The CLI](#the-cli)
   - [forge run](#forge-run)
   - [forge status](#forge-status)
   - [forge status --graph](#forge-status---graph)
   - [forge plugin restart](#forge-plugin-restart)
8. [Writing Plugins](#writing-plugins)
   - [Rust Plugin (Shape A — Server)](#rust-plugin-shape-a--server)
   - [Python Plugin (Shape A — Server)](#python-plugin-shape-a--server)
   - [Rust Plugin (Shape B — Managed Subprocess)](#rust-plugin-shape-b--managed-subprocess)
9. [Plugin SDK Reference](#plugin-sdk-reference)
   - [Plugin Trait](#plugin-trait)
   - [Capability](#capability)
   - [PluginServer](#pluginserver)
   - [KernelClient (calling other plugins)](#kernelclient-calling-other-plugins)
   - [InvokeContext](#invokecontext)
   - [PluginError](#pluginerror)
10. [The Gateway API](#the-gateway-api)
    - [HTTP Endpoints](#http-endpoints)
    - [gRPC Endpoints](#grpc-endpoints)
11. [Embedding the Kernel](#embedding-the-kernel)
12. [Environment Variables](#environment-variables)
13. [Running the Example Backend](#running-the-example-backend)
14. [Testing Your Setup](#testing-your-setup)
15. [Lifecycle & Restart Behavior](#lifecycle--restart-behavior)
16. [Troubleshooting](#troubleshooting)

---

## What is Forge?

Forge is a polyglot backend microkernel. It spawns, manages, and routes requests between plugin processes written in any language that speaks gRPC.

**Key idea:** Plugins register *capabilities* (named, versioned operations). The kernel routes invocations to the right plugin by capability name. Plugins never know about each other — they only talk to the kernel. This lets you mix languages, deploy independently, and test in isolation.

Forge is **not** an HTTP framework, a service mesh, or an event bus. It's a thin lifecycle kernel.

**Three ways to use it:**

| Shape | Description |
|-------|-------------|
| **CLI** | `forge run` — the full daemon: kernel + gateway + plugin manager |
| **Embedded** | `forge_backend::Kernel` — embed the kernel in your own Rust program without any HTTP/gRPC listeners |
| **SDK** | `forge-plugin-sdk-rust` — write plugins in Rust that plug into any Forge kernel |

---

## Installation

### From the release page

```bash
# Quick install (requires curl and sha256sum):
curl -fsSL https://github.com/patchyevolve/forge-backend-framework/releases/download/v1.0.0/install.sh | sh

# This installs the `forge` binary to ~/.local/bin/forge.
# Run as root to install to /usr/local/bin/forge instead.
```

### From source

```bash
git clone https://github.com/patchyevolve/forge-backend-framework.git
cd forge-backend-framework
cargo build --release -p forge-cli
# Binary at target/release/forge
```

### Verify it works

```bash
forge --version
# Should print: forge 1.0.0
```

---

## Directory Layout

A typical Forge project looks like this:

```
my-forge-app/
├── forge.toml               # Kernel configuration
└── plugins/                 # Plugin manifests (scanned by the kernel)
    ├── echo-rs/
    │   └── plugin.forge.toml
    ├── auth-jwt/
    │   └── plugin.forge.toml
    └── data-sqlite/
        └── plugin.forge.toml
```

The plugin binaries themselves can live anywhere on disk. The manifest tells the kernel how to find them.

---

## Kernel Configuration (forge.toml)

This is the main config file. Forge looks for it with `forge run --config forge.toml` (defaults to `forge.toml` in the current directory).

```toml
forge_config_version = "1.0"          # Must be "1.x"

[gateway]
grpc_bind = "127.0.0.1:9090"         # gRPC listener address (default: 127.0.0.1:9090)
http_bind = "127.0.0.1:9091"         # HTTP health/status/invoke endpoint (default: 127.0.0.1:9091)
tls = false                          # Enable TLS for both listeners (default: false)
tls_cert_path = "/etc/forge/cert.pem" # Required if tls = true
tls_key_path = "/etc/forge/key.pem"  # Required if tls = true

[log]
level = "info"                       # Log level: trace, debug, info, warn, error (default: info)

[plugins]
manifest_dir = "./plugins"           # Directory to scan for plugin.forge.toml files (default: ./plugins)
watch = false                        # Hot-reload plugins when manifests change (default: false)
```

### Minimal config

A completely minimal `forge.toml` needs nothing — all fields have defaults:

```toml
forge_config_version = "1.0"
```

This gives you gRPC on `127.0.0.1:9090`, HTTP on `127.0.0.1:9091`, no TLS, `info` logging, and looks for plugins in `./plugins`.

---

## Plugin Manifests (plugin.forge.toml)

Each plugin gets its own directory with a `plugin.forge.toml` manifest that tells the kernel how to connect and what capabilities it provides.

### Shape A — Server plugin (plugin listens on a socket)

```toml
forge_manifest_version = "1.0"        # Must be "1.x"

[plugin]
name = "echo-rs"                      # Unique plugin name
version = "0.1.0"                     # Plugin version
description = "Echo plugin"           # Human-readable description
protocol_version = "1.0"              # Forge protocol version (must be "1.x")

[transport]
shape = "server"                      # Plugin connects as a gRPC server
address = "http://127.0.0.1:50051"    # Where the plugin listens (TCP or unix://)

[lifecycle]
restart_policy = "on-failure"         # "on-failure" | "always" | "never" (default: "on-failure")
restart_backoff_initial_ms = 500      # Initial backoff before first restart (default: 500)
restart_backoff_max_ms = 30000        # Max backoff cap (default: 30000)
restart_max_attempts = 5              # Max consecutive restarts before giving up (default: 5)
health_check_interval_ms = 5000       # How often to ping the plugin (default: 5000)
health_check_failure_threshold = 3    # Failures before marking unhealthy (default: 3)
drain_grace_period_ms = 10000         # Grace period for in-flight requests on shutdown (default: 10000)

[capabilities]
provides = ["forge.example.echo@1.0"] # What this plugin can do
requires = []                         # What this plugin needs from others
```

### Shape B — Managed Subprocess plugin (kernel spawns it)

```toml
forge_manifest_version = "1.0"

[plugin]
name = "echo-py"
version = "0.1.0"
description = "Python echo plugin"
protocol_version = "1.0"

[transport]
shape = "managed-subprocess"           # Kernel spawns this as a child process
executable = "/usr/bin/python3"        # Path to the executable
args = ["-m", "echo_plugin"]          # Command-line arguments
working_dir = "/opt/plugins/echo-py"   # Working directory (optional)

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["forge.example.echo@1.0"]
requires = []
```

### Capability dependency graph

If a plugin lists `requires = ["forge.example.echo@1.0"]`, the kernel will verify at startup that another plugin provides that capability. Use `forge status --graph` to visualize the dependency graph from your manifests.

---

## Starting Forge

### Step 1: Build or install your plugins

Each plugin is a separate binary or script. For Rust plugins:

```bash
cd my-plugin
cargo build --release
```

### Step 2: Start your plugins

For Shape A (server) plugins, start each one in its own terminal or as a background process:

```bash
FORGE_LISTEN_ADDR="127.0.0.1:50051" ./target/release/forge-plugin-echo-rs &
```

The plugin listens on the address you give it. This must match the address in its `plugin.forge.toml`.

### Step 3: Start the Forge kernel

```bash
forge run --config forge.toml
```

Forge will:
1. Load `forge.toml`
2. Scan the manifest directory for all `plugin.forge.toml` files
3. Connect to each plugin and register its capabilities
4. Start health-check pings for each plugin
5. Open the gRPC gateway (port 9090) and HTTP gateway (port 9091)
6. Block until you press Ctrl+C, then drain all plugins gracefully

### Full start-up sequence

```bash
# 1. Start plugins (each in their own terminal or backgrounded)
FORGE_LISTEN_ADDR="127.0.0.1:50051" ./target/release/forge-plugin-echo-rs &
FORGE_LISTEN_ADDR="127.0.0.1:50052" ./target/release/forge-plugin-auth-jwt &

# 2. Wait for plugins to be ready
sleep 2

# 3. Start forge
forge run --config forge.toml
```

---

## The CLI

### forge run

Start the Forge kernel daemon.

```bash
forge run                          # Uses forge.toml in current directory
forge run --config /etc/forge/forge.toml  # Custom config path
```

Press Ctrl+C to shut down gracefully.

### forge status

Show the current state of the kernel and all connected plugins.

```bash
forge status
```

Example output:

```
=== Forge Kernel Status ===

Plugins:
  echo-rs  [Running]
  auth-jwt  [Running]

Capabilities:
  forge.example.echo@1.0  (provided by echo-rs)
  forge.auth.authenticate@1.0  (provided by auth-jwt)
```

### forge status --graph

Visualize the capability dependency graph from your manifests. Does **not** need a running kernel.

```bash
forge status --graph
```

Example output:

```
=== Capability Dependency Graph ===

  echo-rs
    provides:
      - forge.example.echo@1.0

  auth-jwt
    provides:
      - forge.auth.authenticate@1.0

  http-router
    requires:
      - forge.auth.authenticate@1.0  → auth-jwt
      - forge.example.echo@1.0  → echo-rs
    provides:
      - forge.example.http-route@1.0
```

### forge plugin restart

Restart a specific plugin by name without restarting the whole kernel.

```bash
forge plugin restart echo-rs
```

---

## Writing Plugins

### Rust Plugin (Shape A — Server)

This is the standard pattern. The plugin starts its own gRPC server, the kernel connects to it.

**Cargo.toml:**

```toml
[dependencies]
forge-plugin-sdk-rust = "1.0"
tokio = { version = "1", features = ["full"] }
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

**src/main.rs:**

```rust
use forge_plugin_sdk_rust::{Capability, InvokeContext, InvokeResult, PluginError, PluginServer};

struct MyPlugin;

#[forge_plugin_sdk_rust::async_trait]
impl forge_plugin_sdk_rust::Plugin for MyPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("my:action", "1.0.0")]
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "my:action" => {
                let input = String::from_utf8_lossy(&ctx.payload);
                let result = format!("you said: {input}");
                Ok(result.into_bytes())
            }
            other => Err(PluginError::not_found(format!("unknown: {other}"))),
        }
    }

    async fn health_check(&self) -> bool {
        true
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // The kernel passes this addr as an env var, or defaults to 127.0.0.1:50051
    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:50051");
    }

    PluginServer::new(MyPlugin).serve_shape_a().await
}
```

**plugin.forge.toml:**

```toml
forge_manifest_version = "1.0"

[plugin]
name = "my-plugin"
version = "0.1.0"
description = "My custom plugin"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:50051"

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["my:action@1.0"]
requires = []
```

### Python Plugin (Shape A — Server)

Forge's protocol is gRPC, so any language with gRPC support can be a plugin.

**plugin.py** (using the generated protobuf stubs):

```python
import grpc
from concurrent import futures
import forge_plugin_v1_pb2 as pb2
import forge_plugin_v1_pb2_grpc as pb2_grpc

class EchoPlugin(pb2_grpc.ForgePluginServicer):
    def Register(self, request, context):
        return pb2.RegisterResponse(
            plugin_protocol_version="1.0",
            capabilities=[pb2.Capability(name="forge.example.echo", version="1.0")]
        )

    def Invoke(self, request, context):
        payload = request.payload.upper()
        return pb2.InvokeResponse(
            request_id=request.request_id,
            result=pb2.invoke_response.Result(payload=payload)
        )

    def HealthCheck(self, request, context):
        return pb2.HealthCheckResponse(healthy=True)

    def Drain(self, request, context):
        return pb2.DrainResponse()

server = grpc.server(futures.ThreadPoolExecutor(max_workers=10))
pb2_grpc.add_ForgePluginServicer_to_server(EchoPlugin(), server)
listen_addr = os.environ.get("FORGE_LISTEN_ADDR", "127.0.0.1:50051")
server.add_insecure_port(listen_addr)
server.start()
server.wait_for_termination()
```

**plugin.forge.toml:**

```toml
forge_manifest_version = "1.0"

[plugin]
name = "echo-py"
version = "0.1.0"
description = "Python echo plugin"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:50052"

[capabilities]
provides = ["forge.example.echo@1.0"]
requires = []
```

### Rust Plugin (Shape B — Managed Subprocess)

The kernel spawns the plugin as a child process instead of the plugin running its own server. This works with the same SDK — the SDK auto-detects whether it's running as a managed subprocess or standalone.

**Cargo.toml:** Same as Shape A.

**src/main.rs:** Same as Shape A.

**plugin.forge.toml:**

```toml
forge_manifest_version = "1.0"

[plugin]
name = "my-plugin"
version = "0.1.0"
description = "Plugin managed as a subprocess"
protocol_version = "1.0"

[transport]
shape = "managed-subprocess"
executable = "/usr/local/bin/my-plugin-binary"
args = []
working_dir = "/opt/my-plugin"

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["my:action@1.0"]
requires = []
```

---

## Plugin SDK Reference

### Plugin Trait

Every plugin must implement this trait:

```rust
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    // REQUIRED:

    /// Advertise the capabilities this plugin provides.
    fn capabilities(&self) -> Vec<Capability>;

    /// Handle an invocation for one of your capabilities.
    /// Return Ok(bytes) on success, Err(PluginError) on failure.
    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult;

    /// Return false to tell the kernel you're unhealthy.
    async fn health_check(&self) -> bool;

    // OPTIONAL:

    /// Called before the kernel force-kills you.
    /// Use this to flush data, close connections, etc.
    async fn on_drain(&self) {}  // default: no-op
}
```

### Capability

Something your plugin can do. Used in `capabilities()`:

```rust
pub struct Capability {
    pub name: String,              // e.g. "forge.example.echo"
    pub version: String,           // e.g. "1.0.0"
    pub input_schema_ref: String,  // JSON Schema URL for input validation
    pub output_schema_ref: String, // JSON Schema URL for output validation
}

impl Capability {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self;
}
```

### PluginServer

Wraps a `Plugin` and serves it over gRPC:

```rust
pub struct PluginServer<P: Plugin> { .. }

impl<P: Plugin> PluginServer<P> {
    /// Create a new server wrapping your plugin.
    pub fn new(plugin: P) -> Self;

    /// Start listening. Reads FORGE_LISTEN_ADDR env var.
    /// Supports TCP (127.0.0.1:50051) and Unix sockets (unix:///tmp/plugin.sock).
    /// Falls back to unix:///tmp/forge-plugin.sock if nothing is set.
    pub async fn serve_shape_a(self) -> anyhow::Result<()>;
}
```

### KernelClient (calling other plugins)

A plugin can call other plugins through the kernel. This is how you build plugin chains:

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
        request_id: &str,  // Pass through from InvokeContext for tracing
    ) -> InvokeResult;
}
```

### InvokeContext

What gets passed to your `invoke` handler:

```rust
pub struct InvokeContext {
    pub request_id: String,               // Unique ID for tracing across plugins
    pub capability: String,               // The capability being invoked
    pub payload: Vec<u8>,                 // Opaque payload (typically JSON)
    pub metadata: HashMap<String, String>, // Key-value metadata from the caller
}
```

### PluginError

Return this from `invoke` when something goes wrong:

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

Error implements `Display` and `std::error::Error`, so you can use `?` and `anyhow::Error` with it.

---

## The Gateway API

Forge exposes two gateway interfaces. Both are just thin translation layers to the internal bus.

### HTTP Endpoints

Base URL: `http://<http_bind>` (default `http://127.0.0.1:9091`)

#### GET /v1/healthz

Simple health check. Returns `200 OK` with body `"ok"`.

```bash
curl http://127.0.0.1:9091/v1/healthz
# → ok
```

#### GET /v1/status

Returns JSON with all connected plugins and registered capabilities.

```bash
curl http://127.0.0.1:9091/v1/status
```

Response:

```json
{
  "plugins": [
    { "name": "echo-rs", "state": "Running" },
    { "name": "auth-jwt", "state": "Running" }
  ],
  "capabilities": [
    { "name": "forge.example.echo", "version": "1.0", "plugin": "echo-rs" },
    { "name": "forge.auth.authenticate", "version": "1.0", "plugin": "auth-jwt" }
  ]
}
```

#### POST /v1/invoke

Invoke a capability by name. Payload is JSON with:

- `capability` — the capability string to invoke
- `payload` — base64-encoded payload bytes
- `metadata` — optional key-value map

```bash
curl -X POST http://127.0.0.1:9091/v1/invoke \
  -H "Content-Type: application/json" \
  -d '{"capability": "forge.example.echo", "payload": "'$(echo -n "hello" | base64)'"}'
```

Response:

```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "payload": "SEVMTE8="
}
```

Or on error:

```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "error": { "code": "NOT_FOUND", "message": "unknown capability: foo" }
}
```

#### POST /v1/plugins/{name}/restart

Restart a specific plugin by name.

```bash
curl -X POST http://127.0.0.1:9091/v1/plugins/echo-rs/restart
```

### gRPC Endpoints

Address: `<grpc_bind>` (default `127.0.0.1:9090`)

The gRPC gateway exposes the same `ForgePlugin` service that plugins use. This means you can call any capability from any gRPC client using the same protobuf types:

```rust
use forge_proto::forge_plugin_client::ForgePluginClient;

let mut client = ForgePluginClient::connect("http://127.0.0.1:9090").await?;
let response = client.invoke(tonic::Request::new(InvokeRequest {
    request_id: uuid.to_string(),
    capability: "forge.example.echo".into(),
    payload: b"hello".to_vec(),
    metadata: HashMap::new(),
})).await?;
```

---

## Embedding the Kernel

If you don't want the full gateway/daemon, you can embed Forge's kernel directly in your Rust program. This gives you the registry, bus, and lifecycle without any HTTP or gRPC listeners.

**Cargo.toml:**

```toml
[dependencies]
forge-backend = "1.0"
tokio = { version = "1", features = ["full"] }
```

**src/main.rs:**

```rust
use forge_backend::bus::Invocation;
use forge_backend::kernel::{Kernel, KernelConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let kernel = Kernel::start(KernelConfig::default());

    // Register an in-process handler (no gRPC needed)
    kernel
        .bus()
        .register_handler("ping", |_inv: Invocation| async move {
            Ok(bytes::Bytes::from_static(b"pong"))
        })
        .await;

    // Dispatch to it
    let result = kernel
        .bus()
        .dispatch(Invocation::simple("ping", vec![]))
        .await?;

    assert_eq!(&result[..], b"pong");
    println!("ping → {}", String::from_utf8_lossy(&result));
    Ok(())
}
```

This is useful for:
- Adding Forge to an existing axum/actix-web server
- Unit-testing plugin logic
- Running in constrained environments where gRPC overhead is unwanted

To embed without gRPC at all:

```toml
forge-backend = { version = "1.0", default-features = false }
```

---

## Environment Variables

### Kernel env vars (override forge.toml)

| Variable | Overrides | Example |
|----------|-----------|---------|
| `FORGE_CONFIG_VERSION` | `forge_config_version` | `1.0` |
| `FORGE_GATEWAY_GRPC_BIND` | `gateway.grpc_bind` | `0.0.0.0:9090` |
| `FORGE_GATEWAY_HTTP_BIND` | `gateway.http_bind` | `0.0.0.0:9091` |
| `FORGE_GATEWAY_TLS` | `gateway.tls` | `true` |
| `FORGE_GATEWAY_TLS_CERT_PATH` | `gateway.tls_cert_path` | `/etc/forge/cert.pem` |
| `FORGE_GATEWAY_TLS_KEY_PATH` | `gateway.tls_key_path` | `/etc/forge/key.pem` |
| `FORGE_LOG_LEVEL` | `log.level` | `debug` |
| `FORGE_PLUGINS_MANIFEST_DIR` | `plugins.manifest_dir` | `/etc/forge/plugins` |
| `FORGE_PLUGINS_WATCH` | `plugins.watch` | `true` |

### Plugin env vars (set by the kernel or user)

| Variable | Purpose | Example |
|----------|---------|---------|
| `FORGE_LISTEN_ADDR` | Address the plugin should listen on (Shape A) | `127.0.0.1:50051` |
| Custom vars | Forwarded from `[env]` in `plugin.forge.toml` | Any key-value |

### Logging env vars (from tracing-subscriber)

| Variable | Purpose | Example |
|----------|---------|---------|
| `RUST_LOG` | Override log level per module | `forge_backend=debug,info` |

---

## Running the Example Backend

The repo includes a complete example backend with multiple plugins and a startup script.

### Build everything

```bash
cd forge-core/
cargo build --release
```

### Start the example

```bash
bash examples/example-backend/start.sh
```

This starts (in order):
1. `forge-plugin-echo-rs` on port 50051
2. `forge-plugin-auth-jwt` on port 50052
3. `forge-plugin-data-sqlite` on port 50053
4. `forge-plugin-http-router` on port 50054
5. The `forge` daemon with `examples/example-backend/forge.toml`

### Try it out

```bash
# Check status
curl http://127.0.0.1:9091/v1/status

# Invoke echo
curl -X POST http://127.0.0.1:9091/v1/invoke \
  -H "Content-Type: application/json" \
  -d '{"capability": "forge.example.echo", "payload": "'$(echo -n "hello" | base64)'"}'
```

Press Ctrl+C in the terminal running `start.sh` to shut everything down gracefully.

---

## Testing Your Setup

### Quick round-trip test

The repo has a ready-made round-trip test:

```bash
bash test_round_trip.sh
```

This:
1. Builds echo-rs and forge-cli
2. Creates temp configs
3. Starts echo-rs
4. Starts forge
5. Checks `/v1/status` for the plugin
6. Invokes the echo capability
7. Verifies the response is uppercased
8. Cleans up

### Offline build test

```bash
bash test_offline_build.sh
```

### Committed backend test

```bash
bash test_committed_backend.sh
```

---

## Lifecycle & Restart Behavior

### Plugin states

A plugin moves through these states:

```
Discovered → Connecting → Registered → Running
                                        ↓
                                    Unhealthy
                                        ↓
                                    Draining
                                        ↓
                                     Stopped
```

### Restart policies

| Policy | Behavior |
|--------|----------|
| `"on-failure"` | Restart only when the plugin crashes or becomes unhealthy |
| `"always"` | Always restart, even after clean shutdown |
| `"never"` | Never restart |

### Backoff on restart

When a plugin crashes repeatedly, the kernel backs off:
1. First restart: immediate (0ms delay)
2. Wait `restart_backoff_initial_ms` (default 500ms)
3. Each failure doubles the wait, capped at `restart_backoff_max_ms` (default 30s)
4. After `restart_max_attempts` (default 5) consecutive failures, the plugin is marked as permanently failed

**Important:** Crash-driven restarts accumulate toward the attempt counter. Operator-initiated restarts (`forge plugin restart`) reset the counter.

### Health checks

The kernel pings each plugin every `health_check_interval_ms` (default 5s). After `health_check_failure_threshold` (default 3) consecutive failures, the plugin is marked unhealthy and the restart policy kicks in.

### Graceful shutdown

When Forge receives Ctrl+C:
1. All plugins receive a `Drain` RPC
2. The kernel waits up to `drain_grace_period_ms` (default 10s) for in-flight requests to complete
3. After the grace period, remaining plugins are force-stopped

---

## Troubleshooting

### Plugin not appearing in status

1. Is the plugin binary running? Check with `ps aux | grep plugin-name`
2. Is the address in `plugin.forge.toml` correct? It must match `FORGE_LISTEN_ADDR`
3. Is the manifest in the right directory? Forge scans the directory from `[plugins] manifest_dir`
4. Check forge's logs with `RUST_LOG=debug forge run --config forge.toml`

### Connection refused

```
ERROR connect: Connection refused (os error 111)
```

The kernel tried to connect to a plugin but nothing was listening. Start the plugin first, then forge.

### Version mismatch

```
ERROR version mismatch: found manifest version 2.0, expected 1.x
```

Your `forge.toml` or `plugin.forge.toml` has an incompatible version field. Both must start with `"1."`.

### Plugin keeps restarting

Check the health check endpoint. If `health_check` in your plugin returns `false`, or the plugin's gRPC server becomes unreachable, the kernel restarts it. Run with `RUST_LOG=debug` to see health check failures.

### No token found when publishing

```bash
cargo login
# Paste your token from https://crates.io/me
```

### "this crate exists but you don't seem to be an owner"

The crate name is taken on crates.io. Rename your crate in `Cargo.toml`:

```toml
[package]
name = "your-unique-name"
```
