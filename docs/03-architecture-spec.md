# Forge Architecture Specification

## 1. What Forge Is

Forge is a **backend operating environment** — a single binary that:

1. Reads a configuration file (`forge.toml`) describing routes and plugins
2. Spawns plugin processes as managed child subprocesses
3. Health-checks, restarts, and drains them automatically
4. Listens on HTTP and gRPC, routing incoming requests to the right plugin
5. Provides built-in services (auth middleware, CORS, rate limiting, TLS, metrics)

The core idea: **plugins register capabilities** (named, versioned operations). Routes map HTTP methods+paths to capabilities. Forge routes requests by capability name, not by plugin identity. Plugins never know about each other — they only talk to Forge.

## 2. High-Level Architecture

```
                    Internet
                       │
                       ▼
              ┌────────────────┐
              │  HTTP Gateway   │  port 9091 (default)
              │  (axum)         │
              └───────┬────────┘
                      │
              ┌───────▼────────┐
              │  gRPC Gateway    │  port 9090 (default)
              │  (tonic)         │
              └───────┬────────┘
                      │
              ┌───────▼────────┐
              │  Forge Kernel    │
              │                  │
              │  ┌──────────┐   │
              │  │ Registry  │   │  maps capability name → plugin handle
              │  └──────────┘   │
              │  ┌──────────┐   │
              │  │   Bus     │   │  dispatches invocations to plugin processes
              │  └──────────┘   │
              │  ┌──────────┐   │
              │  │ Manager   │   │  spawns, health-checks, restarts, drains
              │  └──────────┘   │
              └───────┬────────┘
                      │
         ┌────────────┼────────────┐
         ▼            ▼            ▼
   ┌──────────┐ ┌──────────┐ ┌──────────┐
   │  auth    │ │  health  │ │ example  │
   │ plugin   │ │  plugin  │ │ plugin   │
   └──────────┘ └──────────┘ └──────────┘
   (managed    (managed    (managed
    subprocess) subprocess) subprocess)
```

## 3. Kernel Modules

### 3.1 Registry (`forge/src/registry.rs`)

A concurrent, versioned map from capability name to plugin handle. This is the single source of truth for "what can the system currently do."

Each capability is a tuple: `(name: String, version: String, plugin_handle)`.

Operations:
- **`register(name, version, handle)`** — called when a plugin completes handshake
- **`deregister(name, handle)`** — called when a plugin disconnects or is killed
- **`lookup(name)`** → `Option<PluginHandle>` — called for every invocation
- **`list()`** — introspection for `forge status`

Multiple plugins can register the same capability name (e.g. two instances of a data plugin for sharding). Forge dispatches round-robin across them.

### 3.2 Bus (`forge/src/bus.rs`)

The internal async message router. Every invocation becomes an `Invocation` value:

```rust
struct Invocation {
    capability: String,
    payload: Vec<u8>,
    metadata: HashMap<String, String>,
    request_id: String,
}
```

`bus.dispatch(invocation)` does exactly:

1. **Lookup** — ask Registry to resolve capability → plugin handle
2. **Forward** — send the payload over gRPC to the plugin process
3. **Await** — wait for the plugin's response (with a deadline timeout)
4. **Return** — pass the response back to the caller

The bus does **not** know about HTTP routes, auth, or any business logic. It dispatches by opaque capability name only.

### 3.3 Manager (`forge/src/lifecycle/manager.rs`)

The Manager owns the plugin lifecycle. Every plugin is always in one of these states:

```
Discovered → Connecting → Handshaking → Ready
                                        ↓
                                     Draining
                                        ↓
                                      Stopped
```

- **Discovered**: forge.toml lists this plugin under `[[plugins]]`, but no connection attempted yet
- **Connecting**: Forge sets env vars and spawns the plugin binary as a subprocess
- **Handshaking**: Plugin connects back via gRPC; they exchange capability declarations
- **Ready**: Plugin is registered and can receive invocations
- **Draining**: Graceful shutdown in progress — no new invocations, waiting for in-flight to finish
- **Stopped**: Process terminated, capabilities deregistered

The manager sets these environment variables for every plugin subprocess:

| Variable | Value | Purpose |
|---|---|---|
| `FORGE_LISTEN_ADDR` | `127.0.0.1:<port>` | Where the plugin should bind its gRPC server |
| `FORGE_CALLBACK_ADDR` | Same as LISTEN_ADDR | Legacy compat — same value |
| `FORGE_PLUGIN_NAME` | Plugin name from forge.toml | Identity |

### 3.4 Config Loader (`forge/src/config.rs`)

Loads and validates `forge.toml`. Precedence (highest wins):

1. CLI flags (e.g. `--config path`)
2. Auto-detection (looks for `forge/forge.toml` if default path not found)
3. The config file itself
4. Built-in defaults

### 3.5 Gateway (`forge/src/gateway/`)

Two listeners — HTTP (axum) and gRPC (tonic):

**HTTP Gateway** — translates REST calls into capability invocations:
- Declarative route matching — routes are configured in `forge.toml`
- Auth middleware — before dispatching a route, calls a capability (e.g. `app.auth.verify@1.0`) to validate the Bearer token
- Rate limiting — configurable requests/minute per IP
- CORS — configurable allowed origins
- Static file serving — serves `frontend/` directory
- Prometheus metrics — `/metrics` endpoint
- JSON body parsing — auto-converts HTTP JSON to plugin payload bytes

**gRPC Gateway** — exposes the same capability surface via protobuf. Any gRPC client can call any registered capability through this gateway.

## 4. Request Lifecycle (End to End)

Concrete walkthrough of what happens when a request arrives:

```
1. HTTP POST /login  {"username":"admin","password":"password"}
                         │
2. HTTP Gateway matches route: method=POST, path=/login
   → capability = "app.auth.login@1.0"
   → no auth middleware on this route
                         │
3. Gateway builds Invocation:
   { capability: "app.auth.login@1.0"
     payload: <serialized JSON>,
     request_id: <uuid> }
                         │
4. Bus::dispatch() → Registry::lookup("app.auth.login@1.0")
   → finds auth plugin handle
                         │
5. Bus forwards payload over gRPC to auth plugin process
                         │
6. Auth plugin's invoke() handler:
   - Deserializes username/password from payload
   - Checks credentials (hardcoded: admin/password)
   - Returns { token: "forge-demo-token-admin", user: "admin" }
                         │
7. Response bytes flow back through bus → gateway
   → Gateway wraps in JSON response envelope
   → HTTP 200 with body
```

A more complex example — protected alerts:

```
1. HTTP GET /alerts  Authorization: Bearer <token>
                         │
2. Gateway matches route: method=GET, path=/alerts
   → capability = "app.alerts@1.0"
   → auth = "app.auth.verify@1.0"  ← middleware!
                         │
3. Gateway extracts Bearer token from header
                         │
4. Gateway pre-invokes auth middleware:
   Bus::dispatch("app.auth.verify@1.0", { token: "<bearer>" })
                         │
5. Auth plugin checks token validity
   → returns { valid: true }
                         │
6. Gateway proceeds to main dispatch:
   Bus::dispatch("app.alerts@1.0", ...)
                         │
7. Example plugin returns alerts payload
                         │
8. Gateway wraps and returns HTTP 200
```

If step 5 returns `{ valid: false }`, the gateway returns **HTTP 401 Unauthorized** immediately — the main capability is never invoked.

## 5. Plugin Lifecycle (Managed Subprocess)

All plugins use `shape = "managed-subprocess"`. Forge handles everything:

### Startup Sequence

```
forge run
  │
  ├── Load forge.toml (auto-detect forge/forge.toml if needed)
  │
  ├── For each plugin in [[plugins]]:
  │     │
  │     ├── Assign a random free port
  │     │
  │     ├── Spawn plugin binary as child process
  │     │   with env: FORGE_LISTEN_ADDR, FORGE_PLUGIN_NAME, etc.
  │     │
  │     ├── Wait for plugin to bind its gRPC server
  │     │
  │     ├── Connect to plugin's gRPC address
  │     │
  │     └── Register capabilities → plugin is READY
  │
  ├── Start HTTP + gRPC gateways
  │
  └── Block until Ctrl+C
```

### Health Checks

Forge pings every plugin on a configurable interval (default 5s). If a plugin fails to respond after a configurable threshold (default 3 failures), Forge:

1. Marks the plugin as `Stopped`
2. Deregisters all its capabilities
3. Applies the restart policy

### Restart Policy

When a plugin crashes or becomes unhealthy:

1. **First restart**: immediate (0ms delay)
2. **Subsequent**: exponential backoff — 500ms, 1s, 2s, 4s, 8s … capped at 30s
3. After **5 consecutive failures**, the plugin is permanently stopped
4. Manual restart (`forge plugin restart <name>`) resets the counter

### Graceful Shutdown

On Ctrl+C:

1. All plugins receive a `Drain` RPC
2. Forge waits up to the drain grace period (default 10s) for in-flight requests
3. After the grace period, remaining plugins are force-killed
4. Gateways are shut down

## 6. Configuration Model

Forge uses a single `forge.toml` file that defines everything:

- Gateway (ports, TLS, CORS, rate limiting)
- Routes (method, path, capability mapping, auth middleware)
- Plugins (binary path, capabilities, lifecycle settings)
- Logging

Example structure:

```toml
[gateway]
# HTTP and gRPC bind addresses, TLS, CORS, rate limiting...

[[gateway.routes]]
# method, path, capability, auth...

[[plugins]]
# name, path, capabilities, lifecycle...
```

Forge auto-detects `forge/forge.toml` — users run `forge run` from the project root without any flags.

## 7. What Forge Does Not Do

- **Not a web framework** — Forge does not render HTML, manage sessions, or handle form data
- **Not a service mesh** — no sidecars, no mTLS between plugins
- **Not an event bus** — no pub/sub, no queues, no streaming
- **Not an orchestrator** — Forge runs on one machine; it does not manage clusters
- **Not a data layer** — Forge does not provide storage; plugins do

## 8. Deployment Modes

### Development
```bash
forge init my-project && cd my-project
cargo build --release && forge run
```

### Docker
```bash
docker compose up --build
```

### Production (systemd)
```bash
forge run --daemon
```
