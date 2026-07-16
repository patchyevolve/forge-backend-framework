use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde::Deserialize;

use forge::bus::Bus;
use forge::config::ConfigLoader;
use forge::gateway::Gateway;
use forge::lifecycle::{Manager, PluginState};
use forge::registry::Registry;

#[derive(Deserialize)]
struct StatusResponse {
    plugins: Vec<PluginStatusEntry>,
    capabilities: Vec<CapabilityStatusEntry>,
}

#[derive(Deserialize)]
struct PluginStatusEntry {
    name: String,
    state: String,
}

#[derive(Deserialize)]
struct CapabilityStatusEntry {
    name: String,
    version: String,
    plugin: String,
}

#[derive(Parser)]
#[command(
    name = "forge",
    version = "1.0.0",
    about = "Backend operating environment",
    long_about = "\
Forge is a single binary that spawns, supervises, and routes requests to
plugin processes written in any language (Rust, Python, JS, …).

  forge init my-project     # Bootstrap a new project
  cd my-project
  cargo build --release      # Build plugins
  forge run                  # Start the backend

Then try:

  curl http://localhost:9091/health
  curl http://localhost:9091/version

All plugins use 'managed-subprocess' mode — forge starts them,
health-checks them, restarts them on crash, and drains them on shutdown.
"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Boot up the Forge kernel
    Run {
        /// Path to the forge.toml config
        #[arg(short, long, default_value = "forge.toml")]
        config: PathBuf,
    },
    /// Print kernel and plugin status
    Status {
        /// forge.toml config (used by --graph)
        #[arg(short, long, default_value = "forge.toml")]
        config: PathBuf,
        /// Draw a provides/requires graph from manifests — no need for a running kernel
        #[arg(long)]
        graph: bool,
    },
    /// Manage plugins
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// Scaffold a new plugin
    New {
        #[command(subcommand)]
        template: NewTemplate,
    },
    /// Bootstrap a new project with a Forge-powered backend
    Init {
        /// Project name (creates a directory with this name)
        name: String,
    },
    /// Install a plugin from the Forge registry
    Install {
        /// Name of the plugin to install (e.g. "auth-jwt", "data-sqlite")
        name: String,
        /// Output directory (default: ./forge/plugins/<name>)
        #[arg(short, long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum PluginAction {
    /// Restart a plugin by name
    Restart {
        /// Which plugin to restart
        name: String,
    },
}

#[derive(Subcommand)]
enum NewTemplate {
    /// Create a new plugin skeleton inside forge/plugins/
    Plugin {
        /// Plugin name
        name: String,
        /// Output directory (default: ./forge/plugins/<name>)
        #[arg(short, long)]
        dir: Option<PathBuf>,
    },
}

const DEFAULT_GATEWAY_ADDR: &str = "127.0.0.1:9091";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run { config } => {
            let config = auto_detect_config(config);
            cmd_run(config).await
        }
        Commands::Status { config, graph } => {
            let config = auto_detect_config(config);
            if graph {
                cmd_status_graph(config).await
            } else {
                cmd_status().await
            }
        }
        Commands::Plugin { action } => match action {
            PluginAction::Restart { name } => cmd_plugin_restart(name).await,
        },
        Commands::New { template } => match template {
            NewTemplate::Plugin { name, dir } => cmd_new_plugin(&name, dir),
        },
        Commands::Init { name } => cmd_init(&name),
        Commands::Install { name, dir } => cmd_install(&name, dir),
    }
}

// ---------------------------------------------------------------------------
// Config auto-detection
// ---------------------------------------------------------------------------

fn auto_detect_config(config: PathBuf) -> PathBuf {
    if config == std::path::Path::new("forge.toml") {
        let forge_dir = PathBuf::from("forge/forge.toml");
        if forge_dir.exists() {
            return forge_dir;
        }
    }
    config
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

async fn cmd_run(config_path: PathBuf) -> anyhow::Result<()> {
    // Install the rustls crypto provider. Both aws-lc-rs and ring features may be
    // enabled (tonic pulls in aws-lc-rs via its tls feature, forge-gateway pulls in
    // ring via rustls defaults), so we must explicitly pick one.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let loader = ConfigLoader::new().with_config_path(&config_path);
    let config = loader.load_config()?;

    tracing::info!("forge-cli {} starting", env!("CARGO_PKG_VERSION"));
    tracing::info!(
        "config loaded from {} (forge_config_version {} — OK)",
        config_path.display(),
        config.forge_config_version
    );

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());

    let discovered = loader.discover_plugin_manifests(&config.plugins.manifest_dir);
    for plugin in &discovered {
        tracing::info!(
            "plugin discovered: {} ({})",
            plugin.manifest.plugin.name,
            plugin.manifest_path.display()
        );
    }

    manager.start_all(discovered).await;

    // Watch manifests for changes every 3 seconds
    if config.plugins.watch {
        let w_loader = ConfigLoader::new().with_config_path(&config_path);
        let w_manager = manager.clone();
        let w_manifest_dir = config.plugins.manifest_dir.clone();

        let mut last_mtimes: HashMap<String, std::time::SystemTime> = HashMap::new();
        for p in w_loader.discover_plugin_manifests(&w_manifest_dir) {
            let path = p.manifest_path.to_string_lossy().to_string();
            if let Ok(meta) = std::fs::metadata(&p.manifest_path) {
                if let Ok(t) = meta.modified() {
                    last_mtimes.insert(path, t);
                }
            }
        }

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let plugins = w_loader.discover_plugin_manifests(&w_manifest_dir);
                for p in &plugins {
                    let path = p.manifest_path.to_string_lossy().to_string();
                    let mtime = std::fs::metadata(&p.manifest_path)
                        .and_then(|m| m.modified())
                        .ok();
                    let is_new = !last_mtimes.contains_key(&path);
                    let changed = match (last_mtimes.get(&path), mtime) {
                        (Some(old), Some(new)) => *old != new,
                        _ => false,
                    };
                    if is_new {
                        tracing::info!(
                            "file-watch: new manifest — starting plugin {}",
                            p.manifest.plugin.name
                        );
                        w_manager.start_plugin_if_new(p.clone()).await;
                        if let Some(t) = mtime {
                            last_mtimes.insert(path, t);
                        }
                    } else if changed {
                        tracing::info!(
                            "file-watch: manifest changed — restarting plugin {}",
                            p.manifest.plugin.name
                        );
                        w_manager.restart_plugin(&p.manifest.plugin.name).await;
                        if let Some(t) = mtime {
                            last_mtimes.insert(path, t);
                        }
                    }
                }

                let states = w_manager.list_plugin_states().await;
                for (name, state) in &states {
                    if *state == PluginState::Connecting {
                        tracing::info!(
                            "file-watch: retrying stuck plugin {name} (state {state:?})"
                        );
                        w_manager.retry_plugin_watch(name).await;
                    }
                }
            }
        });
    }

    let gateway = Gateway::new(config, registry, bus, manager.clone());

    tracing::info!("forge-cli ready — accepting connections");

    tokio::select! {
        res = gateway.start() => {
            if let Err(e) = res {
                tracing::error!("gateway error: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutdown signal received — draining plugins");
            manager.shutdown_all().await;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

async fn cmd_status() -> anyhow::Result<()> {
    let body = http_get(DEFAULT_GATEWAY_ADDR, "/v1/status")
        .map_err(|e| anyhow::anyhow!("failed to query kernel status: {e}"))?;
    let status: StatusResponse = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!("failed to parse status response: {e}"))?;

    println!("=== Forge Kernel Status ===\n");

    println!("Plugins:");
    if status.plugins.is_empty() {
        println!("  (none connected)");
    } else {
        for p in &status.plugins {
            println!("  {}  [{}]", p.name, p.state);
        }
    }

    println!();
    println!("Capabilities:");
    if status.capabilities.is_empty() {
        println!("  (none registered)");
    } else {
        for c in &status.capabilities {
            println!("  {}@{}  (provided by {})", c.name, c.version, c.plugin);
        }
    }

    Ok(())
}

async fn cmd_status_graph(config_path: PathBuf) -> anyhow::Result<()> {
    let loader = ConfigLoader::new().with_config_path(&config_path);
    let config = loader.load_config()?;
    let discovered = loader.discover_plugin_manifests(&config.plugins.manifest_dir);

    println!("=== Capability Dependency Graph ===\n");

    let mut provides_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut requires_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut all_plugins: Vec<String> = Vec::new();

    for p in &discovered {
        let name = p.manifest.plugin.name.clone();
        all_plugins.push(name.clone());
        provides_map.insert(name.clone(), p.manifest.capabilities.provides.clone());
        requires_map.insert(name.clone(), p.manifest.capabilities.requires.clone());
    }

    for name in &all_plugins {
        println!("  {name}");
        if let Some(provides) = provides_map.get(name) {
            if !provides.is_empty() {
                println!("    provides:");
                for cap in provides {
                    println!("      - {cap}");
                }
            }
        }
        if let Some(requires) = requires_map.get(name) {
            if !requires.is_empty() {
                println!("    requires:");
                for cap in requires {
                    let providers: Vec<&String> = all_plugins
                        .iter()
                        .filter(|other| {
                            *other != name
                                && provides_map.get(*other).is_some_and(|p| {
                                    p.iter().any(|c| {
                                        let name_only = c.split('@').next().unwrap_or(c);
                                        let required_name = cap.split('@').next().unwrap_or(cap);
                                        name_only == required_name
                                    })
                                })
                        })
                        .collect();
                    if providers.is_empty() {
                        println!("      - {cap}  (unresolved — no provider found)");
                    } else {
                        let provider_names: Vec<&str> =
                            providers.iter().map(|s| s.as_str()).collect();
                        println!("      - {cap}  → {}", provider_names.join(", "));
                    }
                }
            }
        }
        if provides_map.get(name).is_none_or(|p| p.is_empty())
            && requires_map.get(name).is_none_or(|r| r.is_empty())
        {
            println!("    (no capabilities declared)");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugin restart
// ---------------------------------------------------------------------------

async fn cmd_plugin_restart(name: String) -> anyhow::Result<()> {
    let path = format!("/v1/plugins/{name}/restart");
    let body = http_post(DEFAULT_GATEWAY_ADDR, &path, "")
        .map_err(|e| anyhow::anyhow!("failed to restart plugin: {e}"))?;
    println!("{body}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Scaffolding: forge new project
// ---------------------------------------------------------------------------

fn cmd_init(name: &str) -> anyhow::Result<()> {
    let project_dir = PathBuf::from(name);
    if project_dir.exists() {
        anyhow::bail!("directory '{}' already exists", project_dir.display());
    }

    let forge_dir = project_dir.join("forge");
    let plugins_dir = forge_dir.join("plugins");
    std::fs::create_dir_all(plugins_dir.join("auth").join("src"))?;
    std::fs::create_dir_all(plugins_dir.join("health").join("src"))?;
    std::fs::create_dir_all(plugins_dir.join("example").join("src"))?;
    std::fs::create_dir_all(forge_dir.join("data"))?;
    std::fs::create_dir_all(forge_dir.join("config"))?;
    std::fs::create_dir_all(project_dir.join("frontend"))?;

    // ── Placeholder files for empty dirs ────────────────────────────────
    std::fs::write(project_dir.join("frontend/.gitkeep"), b"")?;
    std::fs::write(forge_dir.join("data/.gitkeep"), b"")?;
    std::fs::write(forge_dir.join("config/.gitkeep"), b"")?;

    // ── Workspace Cargo.toml ────────────────────────────────────────────
    let workspace_cargo = r#"[workspace]
resolver = "2"
members = [
    "forge/plugins/auth",
    "forge/plugins/health",
    "forge/plugins/example",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"

[workspace.dependencies]
forge = "1.0"
tokio = { version = "1", features = ["full"] }
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
"#;
    std::fs::write(project_dir.join("Cargo.toml"), workspace_cargo)?;

    // ── forge/forge.toml ────────────────────────────────────────────────
    let forge_toml = r#"forge_config_version = "1.0"

[gateway]
grpc_bind = "127.0.0.1:9090"
http_bind = "127.0.0.1:9091"
tls = false
cors_allowed_origins = ["*"]

[log]
level = "info"

[plugins]
manifest_dir = "plugins"
watch = true

[[gateway.routes]]
method = "GET"
path = "/health"
capability = "app.health@1.0"

[[gateway.routes]]
method = "GET"
path = "/version"
capability = "app.health@1.0"

[[gateway.routes]]
method = "POST"
path = "/login"
capability = "app.auth.login@1.0"

[[gateway.routes]]
method = "POST"
path = "/echo"
capability = "app.example@1.0"

[[gateway.routes]]
method = "GET"
path = "/alerts"
capability = "app.alerts@1.0"
auth = "app.auth.verify@1.0"
"#;
    std::fs::write(forge_dir.join("forge.toml"), forge_toml)?;

    // ── Auth plugin ─────────────────────────────────────────────────────
    let auth_cargo = r#"[package]
name = "forge-plugin-auth"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
forge = { workspace = true }
tokio = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
"#;
    std::fs::write(plugins_dir.join("auth").join("Cargo.toml"), auth_cargo)?;

    let auth_main = r#"use std::collections::HashMap;

use forge::{Capability, InvokeResult, PluginError, PluginServer};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    token: String,
    user: String,
}

#[derive(Deserialize)]
struct VerifyRequest {
    token: String,
}

#[derive(Serialize)]
struct VerifyResponse {
    valid: bool,
    sub: String,
}

struct AuthPlugin;

#[forge::async_trait]
impl forge::Plugin for AuthPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::new("app.auth.login", "1.0.0"),
            Capability::new("app.auth.verify", "1.0.0"),
        ]
    }

    async fn invoke(&self, ctx: forge::InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "app.auth.login" => {
                let req: LoginRequest =
                    serde_json::from_slice(&ctx.payload).map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected username + password: {e}"),
                        details: HashMap::new(),
                    })?;
                if req.password != "password" {
                    return Err(PluginError {
                        code: "INVALID_CREDENTIALS".into(),
                        message: "bad username or password".into(),
                        details: HashMap::new(),
                    });
                }
                let token = format!("forge-demo-token-{}", req.username);
                let resp = LoginResponse {
                    token,
                    user: req.username,
                };
                serde_json::to_vec(&resp).map_err(|e| PluginError {
                    code: "SERIALIZATION_ERROR".into(),
                    message: format!("{e}"),
                    details: HashMap::new(),
                })
            }
            "app.auth.verify" => {
                let req: VerifyRequest =
                    serde_json::from_slice(&ctx.payload).map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected JSON with 'token' field: {e}"),
                        details: HashMap::new(),
                    })?;
                let valid = req.token.starts_with("forge-demo-token-");
                let sub = if valid {
                    req.token.trim_start_matches("forge-demo-token-").to_string()
                } else {
                    String::new()
                };
                let resp = VerifyResponse { valid, sub };
                serde_json::to_vec(&resp).map_err(|e| PluginError {
                    code: "SERIALIZATION_ERROR".into(),
                    message: format!("{e}"),
                    details: HashMap::new(),
                })
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

    let addr = std::env::var("FORGE_LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".into());
    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        unsafe { std::env::set_var("FORGE_LISTEN_ADDR", &addr); }
    }

    tracing::info!("auth plugin starting on {addr}");
    PluginServer::new(AuthPlugin).serve_shape_a().await
}
"#;
    std::fs::write(plugins_dir.join("auth/src/main.rs"), auth_main)?;

    let auth_manifest = r#"forge_manifest_version = "1.0"

[plugin]
name = "auth"
version = "0.1.0"
description = "Authentication — login + token verification"
protocol_version = "1.0"

[transport]
shape = "managed-subprocess"
executable = "./target/release/forge-plugin-auth"

[lifecycle]
restart_policy = "on-failure"
health_check_interval_ms = 5000
health_check_failure_threshold = 3
drain_grace_period_ms = 5000

[capabilities]
provides = ["app.auth.login@1.0", "app.auth.verify@1.0"]
requires = []
"#;
    std::fs::write(plugins_dir.join("auth/plugin.forge.toml"), auth_manifest)?;

    // ── Health plugin ───────────────────────────────────────────────────
    let health_cargo = r#"[package]
name = "forge-plugin-health"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
forge = { workspace = true }
tokio = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
"#;
    std::fs::write(plugins_dir.join("health").join("Cargo.toml"), health_cargo)?;

    let health_main = r#"use std::collections::HashMap;
use std::time::Instant;

use forge::{Capability, InvokeResult, PluginError, PluginServer};
use serde::Serialize;

#[derive(Serialize)]
struct HealthStatus {
    status: String,
    uptime_seconds: u64,
    version: String,
}

struct HealthPlugin {
    started: Instant,
}

#[forge::async_trait]
impl forge::Plugin for HealthPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("app.health", "1.0.0")]
    }

    async fn invoke(&self, ctx: forge::InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "app.health" => {
                let uptime = self.started.elapsed().as_secs();
                let status = HealthStatus {
                    status: "ok".into(),
                    uptime_seconds: uptime,
                    version: env!("CARGO_PKG_VERSION").into(),
                };
                serde_json::to_vec(&status).map_err(|e| PluginError {
                    code: "SERIALIZATION_ERROR".into(),
                    message: format!("{e}"),
                    details: HashMap::new(),
                })
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

    let addr = std::env::var("FORGE_LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:50052".into());
    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        unsafe { std::env::set_var("FORGE_LISTEN_ADDR", &addr); }
    }

    tracing::info!("health plugin starting on {addr}");
    PluginServer::new(HealthPlugin {
        started: Instant::now(),
    })
    .serve_shape_a()
    .await
}
"#;
    std::fs::write(plugins_dir.join("health/src/main.rs"), health_main)?;

    let health_manifest = r#"forge_manifest_version = "1.0"

[plugin]
name = "health"
version = "0.1.0"
description = "Health endpoint — returns status + uptime"
protocol_version = "1.0"

[transport]
shape = "managed-subprocess"
executable = "./target/release/forge-plugin-health"

[lifecycle]
restart_policy = "on-failure"
health_check_interval_ms = 5000
health_check_failure_threshold = 3
drain_grace_period_ms = 5000

[capabilities]
provides = ["app.health@1.0"]
requires = []
"#;
    std::fs::write(
        plugins_dir.join("health/plugin.forge.toml"),
        health_manifest,
    )?;

    // ── Example plugin ──────────────────────────────────────────────────
    let example_cargo = r#"[package]
name = "forge-plugin-example"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
forge = { workspace = true }
tokio = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
"#;
    std::fs::write(
        plugins_dir.join("example").join("Cargo.toml"),
        example_cargo,
    )?;

    let example_main = r##"use forge::{Capability, InvokeResult, PluginError, PluginServer};

struct ExamplePlugin;

#[forge::async_trait]
impl forge::Plugin for ExamplePlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("app.alerts", "1.0.0"), Capability::new("app.example", "1.0.0")]
    }

    async fn invoke(&self, ctx: forge::InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "app.alerts" => {
                let alerts = r#"{"alerts":[
                    {"id":1,"severity":"high","message":"Port scan detected","timestamp":"2026-07-16T09:00:00Z"},
                    {"id":2,"severity":"medium","message":"Failed login attempt","timestamp":"2026-07-16T08:59:00Z"},
                    {"id":3,"severity":"low","message":"DNS query anomaly","timestamp":"2026-07-16T08:58:00Z"}
                ]}"#;
                Ok(alerts.as_bytes().to_vec())
            }
            "app.example" => {
                let text = String::from_utf8_lossy(&ctx.payload);
                Ok(text.to_uppercase().into_bytes())
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

    let addr = std::env::var("FORGE_LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:50053".into());
    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        unsafe { std::env::set_var("FORGE_LISTEN_ADDR", &addr); }
    }

    tracing::info!("example plugin starting on {addr}");
    PluginServer::new(ExamplePlugin).serve_shape_a().await
}
"##;
    std::fs::write(plugins_dir.join("example/src/main.rs"), example_main)?;

    let example_manifest = r#"forge_manifest_version = "1.0"

[plugin]
name = "example"
version = "0.1.0"
description = "Example + demo alerts — showcases capabilities"
protocol_version = "1.0"

[transport]
shape = "managed-subprocess"
executable = "./target/release/forge-plugin-example"

[lifecycle]
restart_policy = "on-failure"
health_check_interval_ms = 5000
health_check_failure_threshold = 3
drain_grace_period_ms = 5000

[capabilities]
provides = ["app.alerts@1.0", "app.example@1.0"]
requires = []
"#;
    std::fs::write(
        plugins_dir.join("example/plugin.forge.toml"),
        example_manifest,
    )?;

    // ── docker-compose.yml ──────────────────────────────────────────────
    let docker_compose = r#"version: "3.8"

services:
  forge:
    image: forge:latest
    build:
      context: ./forge
      dockerfile: Dockerfile
    ports:
      - "9091:9091"
    volumes:
      - ./forge/plugins:/app/plugins
      - ./forge/forge.toml:/app/forge.toml
      - ./forge/data:/app/data
    environment:
      - FORGE_LOG_LEVEL=info
"#;
    std::fs::write(project_dir.join("docker-compose.yml"), docker_compose)?;

    // ── .gitignore ──────────────────────────────────────────────────────
    let gitignore = "target/\n*.swp\n*.sock\n*.pem\n.env\n.direnv/\n";
    std::fs::write(project_dir.join(".gitignore"), gitignore)?;

    // ── README.md ───────────────────────────────────────────────────────
    let readme = format!(
        r#"# {name}

A backend powered by [Forge](https://forge.dev).

## Structure

```
{name}/
├── frontend/       # Your UI (React, Vue, Svelte, …)
├── forge/          # Backend runtime
│   ├── forge.toml  # Configuration
│   ├── plugins/    # Business logic plugins
│   │   ├── auth/   # Login + token verification
│   │   ├── health/ # Health + version info
│   │   ├── example/# Demo alerts + echo
│   │   └── calculator/  # Demo arithmetic (added by forge new plugin)
│   ├── data/       # Persistent storage
│   └── config/     # Instance-specific config
├── Cargo.toml      # Rust workspace for plugin binaries
├── docker-compose.yml
├── .gitignore
└── README.md
```

## Quick start

```bash
# 1. Build all plugins
cargo build --release

# 2. Start forge
forge run
```

### Test the running backend

```bash
# Health
curl http://localhost:9091/health

# Login (credentials: admin / password)
curl -X POST http://localhost:9091/login \
  -H "Content-Type: application/json" \
  -d '{{"username":"admin","password":"password"}}'

# Protected alerts (requires bearer token from login)
curl http://localhost:9091/alerts \
  -H "Authorization: Bearer forge-demo-token-admin"

# Calculator
curl -X POST http://localhost:9091/calc/add \
  -H "Content-Type: application/json" \
  -d '{{"a":10,"b":3}}'
# → {{"payload":{{"result":13}}}}

curl -X POST http://localhost:9091/calc/mul \
  -H "Content-Type: application/json" \
  -d '{{"a":7,"b":6}}'
# → {{"payload":{{"result":42}}}}
```

## Adding a new plugin

```bash
# Scaffold a new plugin
forge new plugin my-feature

# Implement your capability in src/main.rs, then:
cargo build --release
```

Register the capability in `forge/forge.toml`:

```toml
[[gateway.routes]]
method = "GET"
path = "/my-feature"
capability = "app.my_feature@1.0"
```

## Testing

Run unit tests for all plugins:

```bash
cargo test
```

Test a specific plugin:

```bash
cargo test -p {name}-my-feature
```

## Frontend

Place your frontend app in `frontend/`. Forge serves it statically at the root URL during development. See `frontend/index.html` for a working example that calls the calculator, login, and alerts endpoints.

## Deployment

```bash
# Docker
docker compose up --build

# Systemd (example)
forge run --daemon
```

## Reference

- `forge run` — starts the backend
- `forge new plugin <name>` — scaffolds a plugin
- `forge status` — shows running plugins
- `forge --help` — all commands

See https://forge.dev for the full documentation.
"#,
    );
    std::fs::write(project_dir.join("README.md"), readme)?;

    println!("╔══════════════════════════════════════════════════════╗");
    println!("║  {} initialised                   ║", name);
    println!("╚══════════════════════════════════════════════════════╝");
    println!();
    println!("  cd {name}");
    println!("  cargo build --release");
    println!("  forge run");
    println!();
    println!("Your backend is powered by Forge.");
    println!();
    println!("Endpoints to try:");
    println!("  GET  /health      — health check");
    println!("  POST /login       — login (admin / password)");
    println!("  GET  /alerts      — protected alerts (needs Bearer token)");
    println!("  POST /calc/:op    — calculator (add/sub/mul/div/pow)");
    println!();
    println!("Built-in endpoints:");
    println!("  GET  /health         →  health check");
    println!("  GET  /version        →  version info");
    println!("  POST /login          →  get auth token");
    println!("  GET  /alerts         →  demo alerts (requires auth)");
    println!("  POST /echo           →  echo back text");

    Ok(())
}

// ---------------------------------------------------------------------------
// Scaffolding: forge new plugin
// ---------------------------------------------------------------------------

fn cmd_new_plugin(name: &str, dir: Option<PathBuf>) -> anyhow::Result<()> {
    let plugin_dir = match dir {
        Some(d) => d.join(name),
        None => PathBuf::from("forge/plugins").join(name),
    };

    if plugin_dir.exists() {
        anyhow::bail!("directory '{}' already exists", plugin_dir.display());
    }

    let src_dir = plugin_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    // Cargo.toml
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
license = "MIT"

[dependencies]
forge = "1.0"
tokio = {{ version = "1", features = ["full"] }}
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
tracing = "0.1"
tracing-subscriber = {{ version = "0.3", features = ["env-filter"] }}
anyhow = "1"
"#
    );
    std::fs::write(plugin_dir.join("Cargo.toml"), cargo_toml)?;

    // src/main.rs
    let main_rs = r#"use std::collections::HashMap;

use forge::{
    Capability, InvokeContext, InvokeResult, Plugin, PluginError, PluginServer,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Request {
    message: String,
}

#[derive(Serialize)]
struct Response {
    echo: String,
}

struct MyPlugin;

#[forge::async_trait]
impl Plugin for MyPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("my:action", "1.0.0")]
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "my:action" => {
                let req: Request = serde_json::from_slice(&ctx.payload)
                    .map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("parse error: {e}"),
                        details: HashMap::new(),
                    })?;
                Ok(serde_json::to_vec(&Response {
                    echo: format!("you said: {}", req.message),
                })
                .unwrap())
            }
            other => Err(PluginError::not_found(format!("unknown: {other}"))),
        }
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

    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        unsafe { std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:50051"); }
    }

    PluginServer::new(MyPlugin).serve_shape_a().await
}
"#;
    std::fs::write(src_dir.join("main.rs"), main_rs)?;

    // plugin.forge.toml
    let plugin_toml = format!(
        r#"forge_manifest_version = "1.0"

[plugin]
name = "{name}"
version = "0.1.0"
description = "My Forge plugin"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:50051"

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["my:action@1.0"]
requires = []
"#
    );
    std::fs::write(plugin_dir.join("plugin.forge.toml"), plugin_toml)?;

    println!("Created plugin '{}'", plugin_dir.display());
    println!();
    println!("Next steps:");
    println!(
        "  Add members = [\"{}\"] to [workspace] in Cargo.toml",
        plugin_dir.display()
    );
    println!("  cargo build --release  (from project root)");
    println!("  Add a route in forge/forge.toml");
    println!("  forge run");

    Ok(())
}

// ---------------------------------------------------------------------------
// Install plugin from registry
// ---------------------------------------------------------------------------

fn cmd_install(name: &str, dir: Option<PathBuf>) -> anyhow::Result<()> {
    let install_dir = match dir {
        Some(d) => d,
        None => PathBuf::from("plugins").join(name),
    };

    if install_dir.exists() {
        anyhow::bail!("directory '{}' already exists", install_dir.display());
    }

    // Currently supported plugins with their description
    let known_plugins: HashMap<String, (&str, &str)> = HashMap::from([
        (
            "auth-jwt".to_string(),
            (
                "JWT authentication plugin",
                "https://github.com/patchyevolve/forge-backend-framework",
            ),
        ),
        (
            "data-sqlite".to_string(),
            (
                "SQLite data persistence plugin",
                "https://github.com/patchyevolve/forge-backend-framework",
            ),
        ),
        (
            "http-router".to_string(),
            (
                "HTTP route handler plugin",
                "https://github.com/patchyevolve/forge-backend-framework",
            ),
        ),
        (
            "echo-rs".to_string(),
            (
                "Simple echo plugin for testing",
                "https://github.com/patchyevolve/forge-backend-framework",
            ),
        ),
    ]);

    match known_plugins.get(name) {
        Some((description, repo)) => {
            std::fs::create_dir_all(&install_dir)?;

            // Create the plugin.forge.toml manifest based on the plugin type
            let manifest = match name {
                "auth-jwt" => {
                    r#"forge_manifest_version = "1.0"

[plugin]
name = "auth-jwt"
version = "0.1.0"
description = "JWT authentication plugin"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:50052"

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["forge.auth.verify@1.0"]
requires = []
"#
                }
                "data-sqlite" => {
                    r#"forge_manifest_version = "1.0"

[plugin]
name = "data-sqlite"
version = "0.1.0"
description = "SQLite data persistence plugin"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:50053"

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["forge.data.query@1.0", "forge.data.write@1.0"]
requires = []
"#
                }
                "http-router" => {
                    r#"forge_manifest_version = "1.0"

[plugin]
name = "http-router"
version = "0.1.0"
description = "HTTP route handler plugin"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:50054"

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["forge.example.http-route@1.0"]
requires = ["forge.auth.verify@1.0"]
"#
                }
                _ => {
                    r#"forge_manifest_version = "1.0"
[plugin]
name = "echo-rs"
version = "0.1.0"
description = "Echo plugin"
protocol_version = "1.0"
[transport]
shape = "server"
address = "http://127.0.0.1:50051"
[lifecycle]
restart_policy = "on-failure"
[capabilities]
provides = ["forge.example.echo@1.0"]
requires = []
"#
                }
            };

            std::fs::write(install_dir.join("plugin.forge.toml"), manifest)?;

            println!(
                "Plugin manifest for '{}' created at {}",
                name,
                install_dir.display()
            );
            println!("  Description: {description}");
            println!("  Repository: {repo}");
            println!();
            println!("This plugin needs to be built from source.");
            println!("See {repo} for build instructions.");
            println!();
            println!("After building, place the binary in your PATH and update the");
            println!("manifest's [transport] section with the correct address.");
        }
        None => {
            // Unknown plugin — create a minimal manifest as a starting point
            std::fs::create_dir_all(&install_dir)?;

            let manifest = format!(
                r#"forge_manifest_version = "1.0"

[plugin]
name = "{name}"
version = "0.1.0"
description = "Plugin: {name}"
protocol_version = "1.0"

[transport]
shape = "server"
address = "http://127.0.0.1:50051"

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["{name}:action@1.0"]
requires = []
"#
            );
            std::fs::write(install_dir.join("plugin.forge.toml"), manifest)?;

            println!(
                "Created stub manifest for unknown plugin '{name}' at {}",
                install_dir.display()
            );
            println!();
            println!("Known plugins: auth-jwt, data-sqlite, http-router, echo-rs");
            println!("For custom plugins, write the plugin code and update the manifest.");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Raw HTTP helpers (no external HTTP client dependency)
// ---------------------------------------------------------------------------

fn http_get(host: &str, path: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect_timeout(
        &host.parse().map_err(|e| format!("bad addr: {e}"))?,
        Duration::from_secs(5),
    )
    .map_err(|e| format!("connect: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| format!("read: {e}"))?;
    if let Some(body_start) = response.find("\r\n\r\n") {
        Ok(response[body_start + 4..].to_string())
    } else {
        Ok(response)
    }
}

fn http_post(host: &str, path: &str, body: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect_timeout(
        &host.parse().map_err(|e| format!("bad addr: {e}"))?,
        Duration::from_secs(5),
    )
    .map_err(|e| format!("connect: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| format!("read: {e}"))?;
    if let Some(body_start) = response.find("\r\n\r\n") {
        Ok(response[body_start + 4..].to_string())
    } else {
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn init_creates_directory_structure() {
        let dir = std::env::temp_dir().join("forge-test-init");
        let _ = fs::remove_dir_all(&dir);

        let name = dir.to_string_lossy().to_string();
        cmd_init(&name).unwrap();

        assert!(dir.join("forge/forge.toml").exists());
        assert!(dir.join(".gitignore").exists());
        assert!(dir.join("forge/plugins").is_dir());
        assert!(dir.join("frontend").is_dir());
        assert!(dir.join("forge/data").is_dir());
        assert!(dir.join("forge/config").is_dir());

        let toml_content = fs::read_to_string(dir.join("forge/forge.toml")).unwrap();
        assert!(toml_content.contains("forge_config_version"));

        let workspace = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(workspace.contains("forge/plugins"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_fails_if_exists() {
        let dir = std::env::temp_dir().join("forge-test-init-existing");
        let _ = fs::create_dir_all(&dir);
        let name = dir.to_string_lossy().to_string();
        let result = cmd_init(&name);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_plugin_creates_files() {
        let dir = std::env::temp_dir().join("forge-test-new-plugin");
        let _ = fs::remove_dir_all(&dir);

        cmd_new_plugin("test-plugin", Some(dir.clone())).unwrap();

        let plugin_dir = dir.join("test-plugin");
        assert!(plugin_dir.join("Cargo.toml").exists());
        assert!(plugin_dir.join("src/main.rs").exists());
        assert!(plugin_dir.join("plugin.forge.toml").exists());

        let cargo = fs::read_to_string(plugin_dir.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("forge"));

        let main_rs = fs::read_to_string(plugin_dir.join("src/main.rs")).unwrap();
        assert!(main_rs.contains("impl Plugin for MyPlugin"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_plugin_uses_default_dir() {
        let cwd = std::env::current_dir().unwrap();
        let dir = std::env::temp_dir().join("forge-test-default-dir");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_current_dir(&dir).unwrap();

        cmd_new_plugin("test-plugin", None).unwrap();

        assert!(dir.join("forge/plugins/test-plugin").exists());
        assert!(dir.join("forge/plugins/test-plugin/Cargo.toml").exists());

        std::env::set_current_dir(cwd).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_known_plugin_creates_manifest() {
        let dir = std::env::temp_dir().join("forge-test-install");
        let _ = fs::remove_dir_all(&dir);

        cmd_install("auth-jwt", Some(dir.clone())).unwrap();

        assert!(dir.join("plugin.forge.toml").exists());
        let content = fs::read_to_string(dir.join("plugin.forge.toml")).unwrap();
        assert!(content.contains("auth-jwt"));
        assert!(content.contains("forge.auth.verify"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_unknown_plugin_creates_stub() {
        let dir = std::env::temp_dir().join("forge-test-install-unknown");
        let _ = fs::remove_dir_all(&dir);

        cmd_install("custom-thing", Some(dir.clone())).unwrap();

        assert!(dir.join("plugin.forge.toml").exists());
        let content = fs::read_to_string(dir.join("plugin.forge.toml")).unwrap();
        assert!(content.contains("custom-thing"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_plugin_fails_if_exists() {
        let dir = std::env::temp_dir().join("forge-test-install-exists");
        let _ = fs::create_dir_all(&dir);
        let result = cmd_install("auth-jwt", Some(dir));
        assert!(result.is_err());
    }
}
