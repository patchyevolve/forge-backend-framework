# Forge

**A backend operating environment.**

Forge is not a web framework, not a service mesh, and not an event bus. It is a single binary that orchestrates plugin processes, exposes an HTTP/gRPC gateway, and provides everything your backend needs except business logic.

```
Internet
   │
   ▼
Reverse Proxy (optional)
   │
   ▼
┌────────────────────────────────────────────┐
│                Forge                       │
│  ┌──────────┐  ┌──────────┐  ┌─────────┐ │
│  │  Gateway  │  │  Plugin  │  │ Service │ │
│  │  Routing  │  │  Manager │  │ Manager │ │
│  │  Auth     │  │  Health  │  │   (DB,  │ │
│  │  Rate Lim │  │  Restart │  │  Redis) │ │
│  │  TLS      │  │  Discvry │  │         │ │
│  │  Metrics  │  │  Draining│  │         │ │
│  └──────────┘  └──────────┘  └─────────┘ │
└────────────────────────────────────────────┘
   │           │            │
   ▼           ▼            ▼
 auth     inventory     PostgreSQL
plugin     plugin
```

## Quick start

```bash
# Install forge (pre-built binary)
# curl -fsSL https://raw.githubusercontent.com/patchyevolve/forge-backend-framework/master/install.sh | sh

# Or build from source
git clone https://github.com/patchyevolve/forge-backend-framework
cd forge-core && cargo build --release
./target/release/forge --help
```

Bootstrap a new project:

```bash
forge init my-project
cd my-project
cargo build --release
forge run                   # make sure forge is on your PATH
```

Your backend is now live at `http://localhost:9091`.
If you have a frontend in `frontend/`, it's served at `http://localhost:9091` too (`static_dir = "frontend"` in forge.toml):

```bash
curl http://localhost:9091/healthz
# {"status":"ok"}

curl -X POST http://localhost:9091/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"password"}'
# {"payload":{"token":"forge-demo-token-admin","user":"admin"}}

curl http://localhost:9091/alerts \
  -H "Authorization: Bearer forge-demo-token-admin"
# {"payload":{"alerts":[...]}}
```

## Project structure

```
my-project/
├── frontend/        # Your UI (React, Vue, Svelte, …)
├── forge/           # Backend runtime
│   ├── forge.toml   # Configuration
│   ├── plugins/     # Business logic plugins
│   │   ├── auth/    # Login + token verification
│   │   ├── health/  # Health + version info
│   │   └── example/ # Demo alerts + echo
│   ├── data/        # Persistent storage
│   └── config/      # Instance-specific config
├── Cargo.toml       # Workspace (Rust plugins)
├── docker-compose.yml
├── .gitignore
└── README.md
```

## Adding a capability

Create a plugin, add a route, rebuild:

```bash
forge new plugin inventory
```

Then add to `forge/forge.toml`:

```toml
[[gateway.routes]]
method = "GET"
path = "/products"
capability = "inventory.list@1.0"
```

Rebuild and run:

```bash
cargo build --release
forge run
```

## Plugin SDK

| Language | Package | Status |
|---|---|---|
| Rust | `forge` crate (`forge::sdk` module) | Stable |
| Python | Example (`plugin.py`) | Reference |

A plugin is a self-contained process that implements the Forge plugin protocol (gRPC). It registers capabilities at startup and invokes them on demand.

```rust
impl Plugin for MyPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("my.action", "1.0.0")]
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        // Business logic here
        Ok(b"hello".to_vec())
    }
}
```

## Architecture

Forge provides:

- **HTTP/gRPC gateway** — route requests to plugins
- **Declarative routing** — configure routes in `forge.toml`
- **Auth hooks** — call a plugin to verify every request
- **Rate limiting** — per-IP throttling
- **TLS termination** — HTTPS for both HTTP and gRPC
- **Plugin lifecycle** — spawn, health-check, restart, drain
- **Crash recovery** — auto-restart with exponential backoff
- **Managed subprocesses** — forge spawns and supervises plugins
- **Round-robin dispatch** — load-balance across plugin instances
- **Prometheus metrics** — `/metrics` endpoint
- **CORS** — configurable origins
- **File watching** — hot-reload plugin manifests

## Deployment

```bash
# Docker
docker compose up --build

# systemd
sudo cp systemd/forge.service /etc/systemd/system/
sudo systemctl enable forge
sudo systemctl start forge

# Bare metal
forge run
```

## Performance

| Operation | Latency (p50) |
|---|---|
| In-process dispatch | 4.7 µs |
| gRPC dispatch | 117 µs |
| Registry lookup | 1.5 µs |
| Plugin startup | 1 ms |
| Restart after crash | 55 ms |

Benchmarked on AMD Ryzen 7 7735HS, release mode.

## Status

v1.0.0 — API stable.

## Documentation

| Guide | File | Description |
|---|---|---|
| Architecture | [docs/03-architecture-spec.md](docs/03-architecture-spec.md) | How Forge works internally — kernel modules, request lifecycle, state machine |
| Plugin Development | [docs/07-plugin-developers-guide.md](docs/07-plugin-developers-guide.md) | Writing plugins with the Rust SDK — Plugin trait, capabilities, KernelClient |
| Usage Guide | [docs/10-USAGE-GUIDE.md](docs/10-USAGE-GUIDE.md) | CLI reference, forge.toml config, HTTP API, lifecycle, troubleshooting |
| Building a System | [docs/11-BUILDING-A-SYSTEM.md](docs/11-BUILDING-A-SYSTEM.md) | Full tutorial: build a multi-plugin system from scratch with auth and deployment |
