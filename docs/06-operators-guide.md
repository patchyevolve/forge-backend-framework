# Forge Operator's Guide

## 0. Before You Start

You need a Linux or macOS machine with `curl` and — if building from source — a Rust toolchain (`rustup`).

If you want to see Forge do something in thirty seconds, skip to §3 (Quickstart Demo).

---

## 1. Install

### 1.1 Build from source (current recommendation)

```bash
git clone https://github.com/patchyevolve/forge-backend-framework
cd forge-core
cargo build --release -p forge-cli
sudo cp target/release/forge /usr/local/bin/
```

Verify:

```bash
forge --version
# forge 1.0.0
```

### 1.2 Bootstrap a new project

```bash
forge init my-project
cd my-project
cargo build --release
forge run
```

That's it. Your backend is running at `http://localhost:9091`.

---

## 2. Project Layout

```
my-project/
├── frontend/          # Your UI (served statically by Forge)
├── forge/
│   ├── forge.toml     # Main configuration
│   ├── plugins/       # Plugin source code (Rust crates)
│   │   ├── auth/
│   │   ├── health/
│   │   ├── example/
│   │   └── calculator/
│   └── data/          # Persistent storage
├── Cargo.toml         # Workspace root
├── docker-compose.yml
└── README.md
```

All plugins use `managed-subprocess` mode — Forge spawns them, health-checks them, restarts them on failure, and drains them on shutdown.

---

## 3. Quickstart Demo

After running `forge run`, open another terminal:

```bash
# Health check
curl http://localhost:9091/health
# {"payload":{"status":"ok","uptime_seconds":3,"version":"0.1.0"}}

# Login
curl -X POST http://localhost:9091/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"password"}'
# {"payload":{"token":"forge-demo-token-admin","user":"admin"}}

# Protected alerts
curl http://localhost:9091/alerts \
  -H "Authorization: Bearer forge-demo-token-admin"
# {"payload":{"alerts":[...

# Calculator
curl -X POST http://localhost:9091/calc/add \
  -H "Content-Type: application/json" \
  -d '{"a":10,"b":3}'
# {"payload":{"result":13}}
```

---

## 4. Configuration (forge.toml)

The single configuration file lives at `forge/forge.toml` and defines everything.

### Gateway

```toml
[gateway]
grpc_bind = "127.0.0.1:9090"
http_bind = "127.0.0.1:9091"
cors_allowed_origins = "*"
rate_limit_per_minute = 60
max_body_size = 1048576
```

### Routes

```toml
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
```

The `auth` field specifies a capability to call before dispatching the route. If it returns `valid: false`, the request is rejected with 401.

### Plugins

```toml
[[plugins]]
name = "auth"
path = "target/release/my-project-auth"
capabilities = ["app.auth.login@1.0", "app.auth.verify@1.0"]
```

| Field | Default | Description |
|---|---|---|
| `name` | required | Unique plugin name |
| `path` | required | Path to compiled binary |
| `capabilities` | required | Capability list |
| `args` | `[]` | Command-line arguments |
| `env` | `{}` | Extra environment variables |
| `restart_policy` | `"on-failure"` | `on-failure`, `always`, `never` |

### Logging

```toml
[log]
level = "info"
```

### Full example

See the generated `forge/forge.toml` from `forge init` for a complete working configuration.

---

## 5. Operating Commands

### forge status

Show all plugins and capabilities:

```bash
forge status
```

Output:

```
=== Forge Kernel Status ===

Plugins:
  auth          [Ready]
  health        [Ready]
  example       [Ready]
  calculator    [Ready]

Capabilities:
  app.auth.login@1.0          (provided by auth)
  app.auth.verify@1.0         (provided by auth)
  app.health@1.0              (provided by health)
  ...
```

### forge status --graph

Visualize the capability dependency graph from `forge.toml` — no running kernel needed:

```bash
forge status --graph
```

### forge plugin restart

Restart a specific plugin without restarting the kernel:

```bash
forge plugin restart auth
```

Useful after rebuilding a plugin:

```bash
cargo build --release -p my-project-auth
forge plugin restart auth
```

---

## 6. Adding a New Plugin

```bash
# Scaffold
forge new plugin my-feature

# The plugin is added to the workspace and forge/plugins/my-feature/
# is created with a template main.rs

# Implement the plugin, then build
cargo build --release

# Add a route in forge.toml
[[gateway.routes]]
method = "GET"
path = "/my-feature"
capability = "app.my_feature@1.0"

# Done! forge run picks up the new binary automatically on restart.
```

---

## 7. Security Hardening

Before exposing Forge beyond `127.0.0.1`:

### 7.1 Enable TLS

```toml
[gateway]
http_bind = "0.0.0.0:9091"
grpc_bind = "0.0.0.0:9090"
tls = true
tls_cert_path = "/etc/forge/cert.pem"
tls_key_path = "/etc/forge/key.pem"
```

### 7.2 Restrict CORS

```toml
[gateway]
cors_allowed_origins = "https://myapp.example.com"
```

### 7.3 Set rate limits

```toml
[gateway]
rate_limit_per_minute = 30
```

### 7.4 Plugins are trusted code

Forge does not sandbox plugins. Only run plugins you've built or reviewed yourself.

### 7.5 No outbound network calls

Forge itself makes no outbound network calls after startup. If you see unexpected network activity, it's from a plugin — not the kernel.

---

## 8. Running as a systemd Service

```ini
# /etc/systemd/system/forge.service
[Unit]
Description=Forge Backend
After=network.target

[Service]
Type=simple
WorkingDirectory=/opt/my-project
ExecStart=/usr/local/bin/forge run
Restart=on-failure
RestartSec=5
User=forge

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd --system --no-create-home forge
sudo systemctl daemon-reload
sudo systemctl enable --now forge
journalctl -u forge -f
```

---

## 9. Docker Deployment

The generated `docker-compose.yml` builds everything:

```bash
docker compose up --build
```

Custom `Dockerfile` for production:

```dockerfile
FROM rust:1-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/ .
COPY forge/ ./forge/
EXPOSE 9091
CMD ["forge", "run"]
```

---

## 10. Upgrading

1. Check the running version: `forge --version`
2. Build the new version: `git pull && cargo build --release`
3. Replace the binary: `sudo cp target/release/forge /usr/local/bin/`
4. Restart: `sudo systemctl restart forge` or `Ctrl+C` + `forge run`
5. Verify: `forge status` — all plugins should return to `Ready`

Your `forge.toml` and plugin binaries are not touched during the upgrade.

---

## 11. Observability

### Metrics

```bash
curl http://localhost:9091/metrics
```

Prometheus-format counters for:
- Invocations per capability
- Invocation latency (p50/p95/p99)
- Plugin lifecycle transitions
- Health check failures

### Log level

```bash
# Via config
FORGE_LOG_LEVEL=debug forge run

# Via env var
RUST_LOG=forge=debug forge run
```

---

## 12. Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Plugin not in status | Binary not found at path | Check path in forge.toml, rebuild with `cargo build --release` |
| Plugin keeps restarting | Health check returning false | Check plugin's `health_check()` implementation |
| `Connection refused` | Plugin crashed or path wrong | Run `forge status`, check binary exists |
| 401 on protected route | Missing/expired token | Login first, pass `Authorization: Bearer <token>` |
| `no plugin registered for capability` | Capability name mismatch | Check the `name@version` in forge.toml matches the plugin's `capabilities()` |
| `address already in use` | Port conflict | Check if another process is using port 9090/9091 |
| Config not found | Wrong working directory | Run `forge run` from project root, or use `--config forge/forge.toml` |
