# Building a Multi-Plugin System From Scratch

This tutorial walks through building a complete backend system with Forge — step by step, from zero to a running, testable API. You'll build four plugins, wire them together with routes, add auth middleware, and deploy with Docker.

**Prerequisites:** You have a working Rust toolchain and `forge` installed.

---

## 1. Project Setup

Start with a standard Forge project:

```bash
forge init data-pipeline
cd data-pipeline
```

Look at what was created:

```bash
ls -la
# Cargo.toml    docker-compose.yml    frontend/    forge/    .gitignore    README.md

ls forge/
# forge.toml    config/    data/    plugins/

ls forge/plugins/
# auth/    example/    health/    calculator/
```

The workspace `Cargo.toml` already includes all starter plugins. Build everything:

```bash
cargo build --release
```

You now have a complete, running backend:

```bash
forge run
```

Test it:

```bash
# Terminal 2:
curl http://localhost:9091/health
curl -X POST http://localhost:9091/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"password"}'
```

---

## 2. Understanding the Starter Plugins

Before building our own, understand the four starter plugins that `forge init` provides.

### Health Plugin (`forge/plugins/health`)

Two capabilities:
- `app.health@1.0` — returns `{"status":"ok","uptime_seconds":N}`
- `app.version@1.0` — returns version info

### Auth Plugin (`forge/plugins/auth`)

Two capabilities:
- `app.auth.login@1.0` — accepts `username`/`password`, returns a token
- `app.auth.verify@1.0` — accepts a token, returns `valid: true/false`

Used as auth middleware on protected routes.

### Example Plugin (`forge/plugins/example`)

Two capabilities:
- `app.alerts@1.0` — returns demo alerts (requires valid token)
- `app.echo@1.0` — echoes back whatever payload you send

### Calculator Plugin (`forge/plugins/calculator`)

Five capabilities:
- `app.calculator.add@1.0`, `sub@1.0`, `mul@1.0`, `div@1.0`, `pow@1.0`

Each accepts `{"a":N,"b":N}` and returns `{"result":N}`.

---

## 3. Your First Plugin: A Simple Greeter

Let's add a new plugin that greets users by name.

### 3.1 Scaffold

```bash
forge new plugin greeter
```

This creates `forge/plugins/greeter/` and adds it to the workspace.

### 3.2 Write the plugin

Edit `forge/plugins/greeter/src/main.rs`:

```rust
use std::collections::HashMap;
use forge::sdk::{
    Capability, InvokeContext, InvokeResult, Plugin, PluginError, PluginServer,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct GreetRequest { name: String }

#[derive(Serialize)]
struct GreetResponse { greeting: String }

struct Greeter;

#[forge::sdk::async_trait]
impl Plugin for Greeter {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("app.greeter.hello", "1.0.0")]
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "app.greeter.hello" => {
                let req: GreetRequest = serde_json::from_slice(&ctx.payload)
                    .map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected GreetRequest: {e}"),
                        details: HashMap::new(),
                    })?;
                let resp = GreetResponse {
                    greeting: format!("Hello, {}! Welcome to Forge.", req.name),
                };
                Ok(serde_json::to_vec(&resp).unwrap())
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
    PluginServer::new(Greeter).serve_shape_a().await
}
```

### 3.3 Register the capability in forge.toml

Add to `forge/forge.toml`:

```toml
[[gateway.routes]]
method = "POST"
path = "/hello"
capability = "app.greeter.hello@1.0"

[[plugins]]
name = "greeter"
path = "target/release/data-pipeline-greeter"
capabilities = ["app.greeter.hello@1.0"]
```

### 3.4 Build and test

```bash
cargo build --release -p data-pipeline-greeter
forge run
```

Test:

```bash
curl -X POST http://localhost:9091/hello \
  -H "Content-Type: application/json" \
  -d '{"name":"Alice"}'

# Response:
# {"payload":{"greeting":"Hello, Alice! Welcome to Forge."}}
```

---

## 4. Plugin with Dependencies: An Analytics Plugin

Now build a plugin that calls other plugins. This analytics plugin will:
1. Count how many alerts exist (calls `app.echo@1.0` with a counting query)
2. Track visit counts in memory
3. Return a report

### 4.1 Scaffold

```bash
forge new plugin analytics
```

### 4.2 Write the plugin

```rust
use std::collections::HashMap;
use std::sync::Mutex;
use forge::sdk::{
    Capability, InvokeContext, InvokeResult, KernelClient, Plugin, PluginError, PluginServer,
};
use serde::Serialize;

#[derive(Serialize)]
struct ReportResponse {
    visit_count: u64,
    message: String,
}

struct Analytics {
    visits: Mutex<u64>,
}

#[forge::sdk::async_trait]
impl Plugin for Analytics {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("app.analytics.report", "1.0.0")]
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "app.analytics.report" => {
                // Increment visit counter
                let mut count = self.visits.lock().unwrap();
                *count += 1;
                let current = *count;

                // Call the kernel's echo capability
                let kernel = KernelClient::connect("http://127.0.0.1:9090").await
                    .map_err(|e| PluginError {
                        code: "CONNECT_FAILED".into(),
                        message: e.to_string(),
                        details: HashMap::new(),
                    })?;

                let echo_result = kernel.invoke(
                    "app.echo@1.0",
                    b"hello from analytics".to_vec(),
                    ctx.metadata.clone(),
                    &ctx.request_id,
                ).await;

                let message = match echo_result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        format!("Visits: {}, Echo: {}", current, text)
                    }
                    Err(e) => format!("Visits: {}, Echo failed: {}", current, e.message),
                };

                let resp = ReportResponse {
                    visit_count: current,
                    message,
                };
                Ok(serde_json::to_vec(&resp).unwrap())
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
    PluginServer::new(Analytics { visits: Mutex::new(0) }).serve_shape_a().await
}
```

### 4.3 Register in forge.toml

```toml
[[gateway.routes]]
method = "GET"
path = "/report"
capability = "app.analytics.report@1.0"
auth = "app.auth.verify@1.0"

[[plugins]]
name = "analytics"
path = "target/release/data-pipeline-analytics"
capabilities = ["app.analytics.report@1.0"]
```

Note the `auth` field — this route is protected. Callers must provide a valid Bearer token.

### 4.4 Build and test

```bash
cargo build --release -p data-pipeline-analytics
forge run
```

```bash
# Login to get a token
TOKEN=$(curl -s -X POST http://localhost:9091/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"password"}' | \
  python3 -c "import sys,json;print(json.load(sys.stdin)['payload']['token'])")

# Call the protected report endpoint
curl http://localhost:9091/report \
  -H "Authorization: Bearer $TOKEN"

# First call:
# {"payload":{"visit_count":1,"message":"Visits: 1, Echo: hello from analytics"}}

# Second call:
# {"payload":{"visit_count":2,"message":"Visits: 2, Echo: hello from analytics"}}
```

Each call increments the visit counter. The analytics plugin calls back to the kernel to invoke `app.echo@1.0` on the example plugin.

---

## 5. Protecting Routes with Auth Middleware

The auth middleware in `forge.toml` is how you protect routes. Here's how it works:

```toml
[[gateway.routes]]
method = "GET"
path = "/alerts"
capability = "app.alerts@1.0"
auth = "app.auth.verify@1.0"   # ← this line
```

When a request hits this route, Forge:

1. Extracts the `Authorization: Bearer <token>` header
2. Calls `app.auth.verify@1.0` on the auth plugin with `{"token": "<token>"}`
3. Checks the response for `valid: true`
4. If valid → calls the main capability (`app.alerts@1.0`)
5. If invalid → returns **401 Unauthorized** immediately

### Adding auth to your own routes

```toml
[[gateway.routes]]
method = "POST"
path = "/admin/users"
capability = "app.admin.create_user@1.0"
auth = "app.auth.verify@1.0"
```

### Skipping auth (public routes)

Just omit `auth`:

```toml
[[gateway.routes]]
method = "GET"
path = "/health"
capability = "app.health@1.0"
# no auth → public
```

---

## 6. Frontend Integration

The `frontend/` directory is served statically by Forge at the root URL. Any file you place there is accessible at `http://localhost:9091/<filename>`. Drop in your React, Vue, Svelte, or vanilla HTML app and Forge serves it.

---

## 7. Testing

### Unit tests for a plugin

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_hello() {
        let plugin = Greeter;
        let ctx = InvokeContext {
            request_id: "test".into(),
            capability: "app.greeter.hello".into(),
            payload: serde_json::json!({"name": "Test"}).to_string().into_bytes(),
            metadata: HashMap::new(),
        };
        let result = plugin.invoke(ctx).await.unwrap();
        let resp: GreetResponse = serde_json::from_slice(&result).unwrap();
        assert_eq!(resp.greeting, "Hello, Test! Welcome to Forge.");
    }
}
```

### Run tests

```bash
# All plugins
cargo test

# Specific plugin
cargo test -p data-pipeline-greeter
```

### Integration test with curl

```bash
#!/bin/bash
set -euo pipefail

echo "=== Integration Test ==="

# Health
echo "Health:"
curl -s http://localhost:9091/health | python3 -m json.tool

# Login
echo "Login:"
TOKEN=$(curl -s -X POST http://localhost:9091/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"password"}' | \
  python3 -c "import sys,json;print(json.load(sys.stdin)['payload']['token'])")
echo "Token: $TOKEN"

# Protected alerts
echo "Alerts:"
curl -s http://localhost:9091/alerts \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# Hello
echo "Greeter:"
curl -s -X POST http://localhost:9091/hello \
  -H "Content-Type: application/json" \
  -d '{"name":"Integration"}' | python3 -m json.tool

# Report
echo "Report:"
curl -s http://localhost:9091/report \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

echo "=== All Tests Passed ==="
```

---

## 8. Deployment

### Docker

The generated `docker-compose.yml` builds everything:

```bash
docker compose up --build
```

This builds all plugins and runs Forge inside a container.

### Systemd

Start Forge as a daemon:

```bash
# Install forge binary
sudo cp target/release/forge /usr/local/bin/

# Create a systemd service
sudo tee /etc/systemd/system/forge.service > /dev/null <<'EOF'
[Unit]
Description=Forge Backend
After=network.target

[Service]
Type=simple
WorkingDirectory=/opt/my-project
ExecStart=/usr/local/bin/forge run
Restart=always
User=forge

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl enable forge
sudo systemctl start forge
```

### Production considerations

- Use a reverse proxy (nginx, Caddy) in front of Forge for TLS termination
- Set `cors_allowed_origins` to specific domains, not `*`
- Configure `rate_limit_per_minute` to prevent abuse
- Use `FORGE_LOG_LEVEL=warn` in production to reduce log noise
- Set `RUST_LOG=forge=info,warn` for structured logging

---

## 9. What You've Built

```
                         ┌────────────────────┐
                         │   HTTP Gateway      │
                         │   :9091             │
                         └────────┬───────────┘
                                  │
                         ┌────────▼───────────┐
                         │   Forge Kernel       │
                         │   Registry | Bus     │
                         └──┬─────┬─────┬─────┘
                            │     │     │
               ┌────────────┼─────┼─────┼──────────────┐
               │            │     │     │              │
          ┌────▼───┐  ┌────▼───┐ ┌▼────▼┐  ┌─────────▼──┐
          │ auth   │  │ health │ │example│  │ analytics  │
          │ login  │  │ health │ │alerts │  │  report    │
          │ verify │  │version │ │echo   │  │ (calls     │
          └────────┘  └────────┘ └───────┘  │  echo via  │
           ┌────────┐  ┌──────────┐         │  kernel)   │
           │greeter │  │calculator│         └────────────┘
           │hello   │  │add/sub   │
           └────────┘  │mul/div/  │
                       │pow       │
                       └──────────┘
```

- **5 plugins**, each a separate Rust binary with its own capabilities
- **Auth middleware** protecting specific routes
- **Plugin-to-plugin communication** through the kernel's gRPC gateway
- **10+ API endpoints** served through a single HTTP gateway
- **Docker deployment** with one command
- **Full test suite** — unit tests + integration tests
