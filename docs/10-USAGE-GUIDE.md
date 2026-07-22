# Forge Usage Guide

## Table of Contents

1. [What is Forge?](#what-is-forge)
2. [Installation](#installation)
3. [Quick Start](#quick-start)
4. [Project Structure](#project-structure)
5. [The CLI Reference](#the-cli-reference)
   - [forge init](#forge-init)
   - [forge new plugin](#forge-new-plugin)
   - [forge run](#forge-run)
   - [forge status](#forge-status)
   - [forge plugin restart](#forge-plugin-restart)
   - [forge install](#forge-install)
6. [Configuration (forge.toml)](#configuration-forgetoml)
   - [Gateway Settings](#gateway-settings)
   - [Routes](#routes)
   - [Plugins](#plugins)
   - [Auth Middleware](#auth-middleware)
7. [Plugin Development](#plugin-development)
   - [Scaffolding](#scaffolding)
   - [The Plugin Trait](#the-plugin-trait)
   - [Capabilities](#capabilities)
   - [Request Handling](#request-handling)
   - [Calling Other Plugins](#calling-other-plugins)
8. [The HTTP API](#the-http-api)
   - [Predefined Routes](#predefined-routes)
   - [Custom Routes](#custom-routes)
   - [Response Format](#response-format)
9. [Lifecycle & Restart Behavior](#lifecycle--restart-behavior)
10. [Environment Variables](#environment-variables)
11. [Troubleshooting](#troubleshooting)

---

## What is Forge?

Forge is a **backend operating environment** — a single binary that orchestrates plugin processes, exposes an HTTP/gRPC gateway, and provides everything your backend needs except business logic.

You can think of it like:

- **A process supervisor** — spawns, health-checks, restarts, and drains plugins
- **An API gateway** — routes HTTP requests to plugins by capability name
- **A service bus** — plugins can call other plugins through the kernel
- **A middleware engine** — auth, CORS, rate limiting, TLS

Forge is **not** a web framework, service mesh, or event bus. It's a thin lifecycle kernel.

## Installation

### From source (recommended for development)

```bash
# Prerequisites: Rust toolchain (rustup)
git clone https://github.com/patchyevolve/forge-backend-framework
cd forge-core
cargo build --release

# The binary is at: target/release/forge
# Add to PATH or copy:
sudo cp target/release/forge /usr/local/bin/
```

### Verify

```bash
forge --version
forge --help
```

## Quick Start

```bash
# 1. Bootstrap a new project
forge init my-project
cd my-project

# 2. Build all plugins
cargo build --release

# 3. Start forge
forge run
```

Your backend is now live at `http://localhost:9091`:

```bash
# Health check
curl http://localhost:9091/health
# {"payload":{"status":"ok","uptime_seconds":3,"version":"0.1.0"}}

# Login (returns a bearer token)
curl -X POST http://localhost:9091/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"password"}'
# {"payload":{"token":"forge-demo-token-admin","user":"admin"}}

# Protected alerts (requires the token)
curl http://localhost:9091/alerts \
  -H "Authorization: Bearer forge-demo-token-admin"
# {"payload":{"alerts":[...]}}

# Calculator
curl -X POST http://localhost:9091/calc/add \
  -H "Content-Type: application/json" \
  -d '{"a":10,"b":3}'
# {"payload":{"result":13}}
```

## Project Structure

```
my-project/
├── frontend/          # Your UI (React, Vue, Svelte, vanilla HTML)
│   └── index.html     # Demo page with calculator + login + alerts
│
├── forge/             # Backend runtime
│   ├── forge.toml     # Main configuration (routes, plugins, gateway)
│   ├── plugins/       # Business logic plugins
│   │   ├── auth/      # Login + token verification
│   │   │   ├── Cargo.toml
│   │   │   └── src/main.rs
│   │   ├── health/    # Health + version info
│   │   ├── example/   # Demo alerts + echo
│   │   └── calculator/ # Arithmetic (add/sub/mul/div/pow)
│   ├── data/          # Persistent storage
│   └── config/        # Instance-specific config (overrides)
│
├── Cargo.toml         # Workspace root — builds all plugins
├── docker-compose.yml # Docker deployment
├── .gitignore
└── README.md
```

## The CLI Reference

### forge init

Bootstrap a new Forge project:

```bash
forge init my-project
cd my-project
```

This creates the entire project structure, workspace `Cargo.toml`, all starter plugins, and `forge/forge.toml` with pre-configured routes. You can immediately:

```bash
cargo build --release
forge run
```

### forge new plugin

Scaffold a new plugin inside an existing project:

```bash
# Default: creates at forge/plugins/<name>
forge new plugin my-feature

# Custom path
forge new plugin my-feature --dir plugins/custom
```

Creates `forge/plugins/my-feature/` with `Cargo.toml` and `src/main.rs` pre-configured as a Forge plugin.

### forge run

Start the Forge kernel:

```bash
# Auto-detect: looks for forge/forge.toml, then forge.toml
forge run

# Explicit config path
forge run --config /path/to/forge.toml
```

Forge will:
1. Load the config
2. Spawn all plugins as managed subprocesses
3. Connect and register capabilities
4. Start the HTTP gateway (default port 9091) and gRPC gateway (default port 9090)
5. Block until Ctrl+C, then drain all plugins gracefully

### forge status

Show running plugins and registered capabilities:

```bash
# Requires a running forge instance
forge status

# Visualize the capability dependency graph (no running kernel needed)
forge status --graph
```

Example output:

```
=== Forge Kernel Status ===

Plugins:
  auth          [Ready]
  health        [Ready]
  example       [Ready]
  calculator    [Ready]

Capabilities:
  app.auth.login@1.0     (provided by auth)
  app.auth.verify@1.0    (provided by auth)
  app.health@1.0         (provided by health)
  app.version@1.0        (provided by health)
  app.alerts@1.0         (provided by example)
  app.echo@1.0           (provided by example)
  app.calculator.add@1.0 (provided by calculator)
  app.calculator.sub@1.0 (provided by calculator)
  app.calculator.mul@1.0 (provided by calculator)
  app.calculator.div@1.0 (provided by calculator)
  app.calculator.pow@1.0 (provided by calculator)
```

### forge plugin restart

Restart a specific plugin without restarting the whole kernel:

```bash
forge plugin restart auth
```

Useful after modifying a plugin's code and rebuilding:

```bash
cargo build --release -p <project-name>-auth
forge plugin restart auth
```

## Configuration (forge.toml)

The `forge.toml` file is the single configuration point for the entire backend. It lives at `forge/forge.toml` in your project.

### Gateway Settings

```toml
[gateway]
# Bind addresses
grpc_bind = "127.0.0.1:9090"       # gRPC gateway
http_bind = "127.0.0.1:9091"       # HTTP gateway

# TLS (optional)
tls = false
# tls_cert_path = "/etc/forge/cert.pem"
# tls_key_path = "/etc/forge/key.pem"

# Static file serving (optional, serves a directory at `/static/`)
static_dir = "frontend"

# CORS (comma-separated origins)
cors_allowed_origins = "*"

# Rate limiting (requests per minute per IP)
rate_limit_per_minute = 60

# Max request body size in bytes
max_body_size = 1048576
```

### Routes

Routes map HTTP methods and paths to plugin capabilities:

```toml
[[gateway.routes]]
method = "GET"
path = "/health"
capability = "app.health@1.0"

[[gateway.routes]]
method = "GET"
path = "/version"
capability = "app.version@1.0"

[[gateway.routes]]
method = "POST"
path = "/login"
capability = "app.auth.login@1.0"

[[gateway.routes]]
method = "POST"
path = "/calc/:op"
capability = "app.calculator.{op}@1.0"
```

Path parameters (like `:op`) are forwarded to the plugin in the request metadata.

### Auth Middleware

Routes can specify an auth capability that's called *before* the main handler:

```toml
[[gateway.routes]]
method = "GET"
path = "/alerts"
capability = "app.alerts@1.0"
auth = "app.auth.verify@1.0"    # Called first; if it rejects, 401
```

How it works:
1. Gateway extracts the `Authorization: Bearer <token>` header
2. Calls `app.auth.verify@1.0` with `{"token": "<token>"}`
3. If the response has `valid: false`, returns **401 Unauthorized**
4. If valid, proceeds to call `app.alerts@1.0`

### Plugins

Define the plugin processes that Forge should spawn:

```toml
[[plugins]]
name = "auth"
path = "target/release/<project-name>-auth"
capabilities = ["app.auth.login@1.0", "app.auth.verify@1.0"]
```

| Field | Description |
|---|---|
| `name` | Unique plugin name for logs, status, restart |
| `path` | Path to the compiled binary (relative to project root) |
| `capabilities` | List of `name@version` this plugin provides |
| `restart_policy` | `on-failure` (default), `always`, or `never` |

### Full Example

```toml
forge_config_version = "1.0"

[gateway]
http_bind = "0.0.0.0:9091"
grpc_bind = "0.0.0.0:9090"
cors_allowed_origins = "*"
rate_limit_per_minute = 60
max_body_size = 1048576

[[gateway.routes]]
method = "GET"
path = "/health"
capability = "app.health@1.0"

[[gateway.routes]]
method = "POST"
path = "/login"
capability = "app.auth.login@1.0"

[[gateway.routes]]
method = "GET"
path = "/alerts"
capability = "app.alerts@1.0"
auth = "app.auth.verify@1.0"

[[gateway.routes]]
method = "POST"
path = "/calc/:op"
capability = "app.calculator.{op}@1.0"

[[plugins]]
name = "health"
path = "target/release/demo-health"
capabilities = ["app.health@1.0", "app.version@1.0"]

[[plugins]]
name = "auth"
path = "target/release/demo-auth"
capabilities = ["app.auth.login@1.0", "app.auth.verify@1.0"]

[[plugins]]
name = "example"
path = "target/release/demo-example"
capabilities = ["app.alerts@1.0", "app.echo@1.0"]

[[plugins]]
name = "calculator"
path = "target/release/demo-calculator"
capabilities = [
    "app.calculator.add@1.0",
    "app.calculator.sub@1.0",
    "app.calculator.mul@1.0",
    "app.calculator.div@1.0",
    "app.calculator.pow@1.0",
]

[log]
level = "info"
```

## Plugin Development

### Scaffolding

```bash
cd your-project
forge new plugin my-feature
```

This creates:

```
forge/plugins/my-feature/
├── Cargo.toml    # Package named <project-name>-<plugin-name>
└── src/
    └── main.rs   # Pre-filled with Plugin trait boilerplate
```

The `Cargo.toml` is automatically added to the workspace in the root `Cargo.toml`.

### The Plugin Trait

Every Forge plugin implements this trait:

```rust
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    fn capabilities(&self) -> Vec<Capability>;
    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult;
    async fn health_check(&self) -> bool;
    async fn on_drain(&self) {}
}
```

### Capabilities

A capability is a named, versioned operation your plugin can perform:

```rust
fn capabilities(&self) -> Vec<Capability> {
    vec![
        Capability::new("app.my_feature", "1.0.0"),
    ]
}
```

Convention: `<domain>.<name>@<version>` where domain is:
- `app` for application-specific capabilities
- `builtin` for kernel built-ins
- `forge` for official plugins

### Request Handling

Receive JSON, return JSON:

```rust
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct MyInput { message: String }

#[derive(Serialize)]
struct MyOutput { response: String }

async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
    let input: MyInput = serde_json::from_slice(&ctx.payload)
        .map_err(|e| PluginError {
            code: "INVALID_PAYLOAD".into(),
            message: e.to_string(),
            details: HashMap::new(),
        })?;

    let output = MyOutput {
        response: format!("you said: {}", input.message),
    };

    Ok(serde_json::to_vec(&output).unwrap())
}
```

### Calling Other Plugins

Use `KernelClient` to call capabilities through the kernel:

```rust
use forge::sdk::KernelClient;

async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
    let kernel = KernelClient::connect("http://127.0.0.1:9090").await
        .map_err(|e| PluginError {
            code: "CONNECT_FAILED".into(),
            message: e.to_string(),
            details: HashMap::new(),
        })?;

    let response = kernel.invoke(
        "app.auth.verify@1.0",
        serde_json::json!({"token": "..."}).to_string().into_bytes(),
        ctx.metadata.clone(),
        &ctx.request_id,
    ).await?;

    Ok(response)
}
```

## The HTTP API

### Predefined Routes

Forge ships with these built-in routes (served by the starter plugins):

| Method | Path | Capability | Auth | Description |
|---|---|---|---|---|
| `GET` | `/health` | `app.health@1.0` | No | Health check + uptime |
| `GET` | `/version` | `app.version@1.0` | No | Version info |
| `POST` | `/login` | `app.auth.login@1.0` | No | Get auth token |
| `GET` | `/alerts` | `app.alerts@1.0` | Yes | Protected alerts |
| `POST` | `/echo` | `app.echo@1.0` | No | Echo request body |
| `POST` | `/calc/add` | `app.calculator.add@1.0` | No | Add two numbers |
| `POST` | `/calc/sub` | `app.calculator.sub@1.0` | No | Subtract |
| `POST` | `/calc/mul` | `app.calculator.mul@1.0` | No | Multiply |
| `POST` | `/calc/div` | `app.calculator.div@1.0` | No | Divide |
| `POST` | `/calc/pow` | `app.calculator.pow@1.0` | No | Power |

### Custom Routes

Add your own in `forge.toml`:

```toml
[[gateway.routes]]
method = "GET"
path = "/users"
capability = "app.users.list@1.0"

[[gateway.routes]]
method = "POST"
path = "/users"
capability = "app.users.create@1.0"
auth = "app.auth.verify@1.0"
```

### Response Format

Successful responses:

```json
{
  "payload": { ... }
}
```

Error responses:

```json
{
  "error": { "code": "NOT_FOUND", "message": "unknown capability" }
}
```

## Lifecycle & Restart Behavior

### Startup Sequence

```
forge run
  │
  ├── Load forge.toml
  │
  ├── For each plugin:
  │     │
  │     ├── Assign random free port
  │     ├── Set env vars (FORGE_LISTEN_ADDR, etc.)
  │     ├── Spawn plugin binary
  │     ├── Wait for gRPC connection
  │     ├── Receive capability registration
  │     └── Plugin is READY
  │
  ├── Start HTTP gateway
  ├── Start gRPC gateway
  └── Block until Ctrl+C
```

### Plugin States

```
Discovered → Connecting → Handshaking → Ready → (serve requests)
                                          ↓
                                       Draining → Stopped
```

### Health Checks

Forge pings each plugin every **5 seconds**. After **3 consecutive failures**:

1. Plugin is marked `Stopped`
2. Capabilities are deregistered
3. Restart policy is applied

### Restart Backoff

| Attempt | Delay |
|---|---|
| 1 | 0ms (immediate) |
| 2 | 500ms |
| 3 | 1s |
| 4 | 2s |
| 5 | 4s |
| 6+ | Capped at 30s |
| After 5 failures | Permanently stopped |

Manual restart (`forge plugin restart <name>`) resets the counter.

### Graceful Shutdown

On Ctrl+C:
1. All plugins receive a `Drain` RPC
2. Forge waits **10 seconds** for in-flight requests
3. Remaining plugins are force-killed
4. Gateways shut down

## Environment Variables

### Set by Forge for plugins

| Variable | Value | Purpose |
|---|---|---|
| `FORGE_LISTEN_ADDR` | `127.0.0.1:<port>` | gRPC server bind address |
| `FORGE_CALLBACK_ADDR` | `127.0.0.1:<port>` | Same as LISTEN_ADDR |
| `FORGE_PLUGIN_NAME` | Plugin name from config | Identity |

### Config overrides

| Variable | Override | Example |
|---|---|---|
| `FORGE_CONFIG_VERSION` | `forge_config_version` | `1.0` |
| `FORGE_GATEWAY_GRPC_BIND` | `gateway.grpc_bind` | `0.0.0.0:9090` |
| `FORGE_GATEWAY_HTTP_BIND` | `gateway.http_bind` | `0.0.0.0:9091` |
| `FORGE_GATEWAY_TLS` | `gateway.tls` | `true` |
| `FORGE_LOG_LEVEL` | `log.level` | `debug` |

### Logging

| Variable | Purpose |
|---|---|
| `RUST_LOG` | Per-module log filtering (e.g., `forge=debug,info`) |
| `FORGE_LOG_LEVEL` | Global log level (trace, debug, info, warn, error) |

## Troubleshooting

### Plugin not appearing in status

1. Is it built? `cargo build --release`
2. Check the path in `forge.toml` — it should be relative to your project root
3. Run with `FORGE_LOG_LEVEL=debug forge run` to see connection attempts
4. The plugin binary must exist at the configured path

### Plugin keeps restarting

The health check is failing. Run with `RUST_LOG=debug forge run` to see:

```
DEBUG lifecycle::manager: health check failed for plugin <name>
```

Check your plugin's `health_check()` implementation — if it's returning `false`, fix it.

### Connection refused

```
ERROR connect: Connection refused
```

This shouldn't happen with `managed-subprocess` plugins (Forge spawns them). If it does, check:

- The binary path in `forge.toml` is correct
- The binary exists and is executable: `ls -la target/release/<name>`
- No port conflicts

### 401 on protected routes

The auth middleware is rejecting your request. Make sure you:

1. Login first: `POST /login` with `admin`/`password`
2. Use the returned token: `Authorization: Bearer <token>`
3. The token is not expired (demo tokens don't expire)

### "no plugin registered for capability"

1. Check that your plugin is listed in `forge status`
2. Verify the capability name in your plugin's `capabilities()` matches the route's `capability` field
3. Check `forge.toml` has the correct `name@version` format (e.g., `app.my_feature@1.0`)
