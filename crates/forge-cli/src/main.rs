use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::SystemTime;

use clap::{Parser, Subcommand};
use serde::Deserialize;

use forge_backend::bus::Bus;
use forge_backend::config::ConfigLoader;
use forge_backend::lifecycle::Manager;
use forge_backend::registry::Registry;
use forge_gateway::Gateway;

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
    about = "Polyglot backend microkernel"
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
}

#[derive(Subcommand)]
enum PluginAction {
    /// Restart a plugin by name
    Restart {
        /// Which plugin to restart
        name: String,
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
        Commands::Run { config } => cmd_run(config).await,
        Commands::Status { config, graph } => {
            if graph {
                cmd_status_graph(config).await
            } else {
                cmd_status().await
            }
        }
        Commands::Plugin { action } => match action {
            PluginAction::Restart { name } => cmd_plugin_restart(name).await,
        },
    }
}

async fn cmd_run(config_path: PathBuf) -> anyhow::Result<()> {
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

    // Watch manifests for changes every 3 seconds, restart affected plugins
    if config.plugins.watch {
        let w_loader = ConfigLoader::new().with_config_path(&config_path);
        let w_manager = manager.clone();
        let w_manifest_dir = config.plugins.manifest_dir.clone();
        tokio::spawn(async move {
            let mut last_mtimes: HashMap<String, SystemTime> = HashMap::new();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let plugins = w_loader.discover_plugin_manifests(&w_manifest_dir);
                for p in &plugins {
                    let path = p.manifest_path.to_string_lossy().to_string();
                    let mtime = std::fs::metadata(&p.manifest_path)
                        .and_then(|m| m.modified())
                        .ok();
                    let changed = match (last_mtimes.get(&path), mtime) {
                        (Some(old), Some(new)) => *old != new,
                        _ => true,
                    };
                    if changed {
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

    // Build a map of what each plugin provides and what each requires
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
                    // Find which plugin (if any) provides this capability
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

async fn cmd_plugin_restart(name: String) -> anyhow::Result<()> {
    let path = format!("/v1/plugins/{name}/restart");
    let body = http_post(DEFAULT_GATEWAY_ADDR, &path, "")
        .map_err(|e| anyhow::anyhow!("failed to restart plugin: {e}"))?;
    println!("{body}");
    Ok(())
}

fn http_get(host: &str, path: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect(host).map_err(|e| format!("connect: {e}"))?;
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
    let mut stream = TcpStream::connect(host).map_err(|e| format!("connect: {e}"))?;
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
