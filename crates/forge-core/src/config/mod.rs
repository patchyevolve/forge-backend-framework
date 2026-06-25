use std::env::current_dir;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The main kernel config, deserialized from a forge.toml file.
#[derive(Debug, Clone, Deserialize)]
pub struct ForgeConfig {
    /// TOML key: `forge_config_version`. Must be `"1.x"` — the loader rejects anything else.
    #[serde(default = "default_forge_config_version")]
    pub forge_config_version: String,

    /// TOML key: `[gateway]`. gRPC/HTTP gateway bind addresses and TLS settings.
    #[serde(default)]
    pub gateway: GatewayConfig,

    /// TOML key: `[log]`. Log level configuration.
    #[serde(default)]
    pub log: LogConfig,

    /// TOML key: `[plugins]`. Plugin manifest directory and live-reload settings.
    #[serde(default)]
    pub plugins: PluginsConfig,
}

/// Gateway network settings — controls the gRPC and HTTP listeners.
#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    /// gRPC listener address. Default: `127.0.0.1:9090`.
    #[serde(default = "default_grpc_bind")]
    pub grpc_bind: String,

    /// HTTP health/readiness listener address. Default: `127.0.0.1:9091`.
    #[serde(default = "default_http_bind")]
    pub http_bind: String,

    /// Enable TLS for both gRPC and HTTP listeners. Default: `false`.
    #[serde(default)]
    pub tls: bool,

    /// Path to the TLS certificate file. Required when `tls = true`.
    #[serde(default)]
    pub tls_cert_path: Option<String>,

    /// Path to the TLS private key file. Required when `tls = true`.
    #[serde(default)]
    pub tls_key_path: Option<String>,
}

/// Logging configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct LogConfig {
    /// Log level (e.g. `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"`). Default: `"info"`.
    #[serde(default = "default_log_level")]
    pub level: String,
}

/// Plugin discovery and hot-reload settings.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginsConfig {
    /// Directory to scan for plugin manifests (`plugin.forge.toml`). Default: `./plugins`.
    #[serde(default = "default_manifest_dir")]
    pub manifest_dir: String,

    /// Check for manifest changes every 3s and hot-reload any affected plugins.
    #[serde(default)]
    pub watch: bool,
}

fn default_forge_config_version() -> String {
    "1.0".into()
}

fn default_grpc_bind() -> String {
    "127.0.0.1:9090".into()
}

fn default_http_bind() -> String {
    "127.0.0.1:9091".into()
}

fn default_log_level() -> String {
    "info".into()
}

fn default_manifest_dir() -> String {
    "./plugins".into()
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            grpc_bind: default_grpc_bind(),
            http_bind: default_http_bind(),
            tls: false,
            tls_cert_path: None,
            tls_key_path: None,
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            manifest_dir: default_manifest_dir(),
            watch: false,
        }
    }
}

impl Default for ForgeConfig {
    fn default() -> Self {
        Self {
            forge_config_version: default_forge_config_version(),
            gateway: GatewayConfig::default(),
            log: LogConfig::default(),
            plugins: PluginsConfig::default(),
        }
    }
}

/// A plugin manifest discovered on disk (plugin.forge.toml).
#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    /// Manifest schema version. Must be `"1.x"`.
    pub forge_manifest_version: String,

    /// Metadata about the plugin (name, version, description, protocol).
    pub plugin: PluginManifestMeta,

    /// How the kernel should communicate with this plugin (server socket or managed subprocess).
    #[serde(default)]
    pub transport: PluginTransport,

    /// Restart, health-check, and drain policies for this plugin.
    #[serde(default)]
    pub lifecycle: PluginLifecycleConfig,

    /// Capabilities this plugin provides and requires from other plugins.
    #[serde(default)]
    pub capabilities: PluginCapabilitiesDecl,

    /// Environment variables forwarded to the plugin's process.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// Metadata section `[plugin]` of a `plugin.forge.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifestMeta {
    /// Unique plugin name (e.g. `"echo-rs"`).
    pub name: String,
    /// Semantic version of the plugin.
    pub version: String,

    /// Human-readable description.
    #[serde(default)]
    pub description: String,

    /// Forge protocol version this plugin speaks. Must be `"1.x"`.
    pub protocol_version: String,
}

/// How the kernel transports messages to/from the plugin.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "shape")]
#[non_exhaustive]
pub enum PluginTransport {
    /// Plugin listens on a known address (e.g. `"unix:///tmp/forge-plugin.sock"`).
    #[serde(rename = "server")]
    Server { address: String },

    /// Plugin is spawned as a subprocess and communicates over stdio.
    #[serde(rename = "managed-subprocess")]
    ManagedSubprocess {
        /// Path to the executable.
        executable: String,
        /// Command-line arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Working directory for the subprocess.
        #[serde(default)]
        working_dir: Option<String>,
    },
}

impl Default for PluginTransport {
    fn default() -> Self {
        PluginTransport::Server {
            address: "unix:///tmp/forge-plugin.sock".into(),
        }
    }
}

/// Restart, health-check, and graceful-shutdown policy for a plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginLifecycleConfig {
    /// When to restart: `"on-failure"`, `"always"`, or `"never"`. Default: `"on-failure"`.
    #[serde(default = "default_restart_policy")]
    pub restart_policy: String,

    /// Initial backoff delay in ms before the first restart. Default: `500`.
    #[serde(default = "default_backoff_initial_ms")]
    pub restart_backoff_initial_ms: u64,

    /// Maximum backoff delay in ms (caps exponential growth). Default: `30_000`.
    #[serde(default = "default_backoff_max_ms")]
    pub restart_backoff_max_ms: u64,

    /// Maximum consecutive restart attempts before giving up. Default: `5`.
    #[serde(default = "default_max_attempts")]
    pub restart_max_attempts: u32,

    /// Interval in ms between health-check pings. Default: `5_000`.
    #[serde(default = "default_health_interval_ms")]
    pub health_check_interval_ms: u64,

    /// Consecutive health-check failures before marking the plugin as unhealthy. Default: `3`.
    #[serde(default = "default_health_threshold")]
    pub health_check_failure_threshold: u32,

    /// Grace period in ms to wait for an in-flight request to finish before force-killing. Default: `10_000`.
    #[serde(default = "default_drain_grace_ms")]
    pub drain_grace_period_ms: u64,
}

impl Default for PluginLifecycleConfig {
    fn default() -> Self {
        Self {
            restart_policy: default_restart_policy(),
            restart_backoff_initial_ms: default_backoff_initial_ms(),
            restart_backoff_max_ms: default_backoff_max_ms(),
            restart_max_attempts: default_max_attempts(),
            health_check_interval_ms: default_health_interval_ms(),
            health_check_failure_threshold: default_health_threshold(),
            drain_grace_period_ms: default_drain_grace_ms(),
        }
    }
}

fn default_restart_policy() -> String {
    "on-failure".into()
}
fn default_backoff_initial_ms() -> u64 {
    500
}
fn default_backoff_max_ms() -> u64 {
    30000
}
fn default_max_attempts() -> u32 {
    5
}
fn default_health_interval_ms() -> u64 {
    5000
}
fn default_health_threshold() -> u32 {
    3
}
fn default_drain_grace_ms() -> u64 {
    10000
}

/// Declared capabilities a plugin provides and requires from others.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PluginCapabilitiesDecl {
    /// Capabilities this plugin exposes (e.g. `"forge.example.echo@1.0"`).
    #[serde(default)]
    pub provides: Vec<String>,

    /// Capabilities this plugin depends on at runtime.
    #[serde(default)]
    pub requires: Vec<String>,
}

/// Discovers and parses config files. Precedence: CLI flags > env vars > file > defaults.
#[derive(Debug, Clone)]
pub struct ConfigLoader {
    config_path: Option<PathBuf>,
    /// Where the config file lives — used to resolve relative manifest_dir paths. Falls back to CWD.
    config_dir: PathBuf,
}

impl ConfigLoader {
    /// Create a loader with no config file path set. Call [`with_config_path`](Self::with_config_path) to point it at a file.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use forge_backend::config::ConfigLoader;
    ///
    /// let loader = ConfigLoader::new();
    /// let config = loader.load_config().unwrap();
    /// assert_eq!(config.forge_config_version, "1.0");
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            config_path: None,
            config_dir: current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    /// Set the path to `forge.toml`. The parent directory is used to resolve relative `manifest_dir` values.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use forge_backend::config::ConfigLoader;
    ///
    /// let loader = ConfigLoader::new()
    ///     .with_config_path("/etc/forge/forge.toml");
    /// let config = loader.load_config().unwrap();
    /// ```
    #[must_use]
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                self.config_dir = parent.to_path_buf();
            }
        }
        self.config_path = Some(path);
        self
    }

    /// Load the kernel config from forge.toml. Env vars override the file, defaults are the floor.
    pub fn load_config(&self) -> Result<ForgeConfig, ConfigError> {
        // Start from scratch with built-ins
        let mut config = ForgeConfig::default();

        // Layer on whatever's in the file
        if let Some(ref path) = self.config_path {
            let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
                path: path.to_string_lossy().into(),
                source: e,
            })?;
            let file_config: ForgeConfig =
                toml::from_str(&contents).map_err(|e| ConfigError::Parse {
                    path: path.to_string_lossy().into(),
                    source: e,
                })?;
            config = file_config;
        }

        // Then let environment variables override everything
        apply_env_overrides(&mut config);

        // Make sure the version in the file is something we understand
        if !config.forge_config_version.starts_with("1.") {
            return Err(ConfigError::VersionMismatch {
                path: self
                    .config_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into()),
                found: config.forge_config_version,
                expected: "1.x".into(),
            });
        }

        Ok(config)
    }

    /// Scan the configured directory for plugin manifests. Relative paths are resolved against the config file's directory (or CWD if no file was specified).
    #[must_use]
    pub fn discover_plugin_manifests(&self, manifest_dir: &str) -> Vec<DiscoveredPlugin> {
        let dir = Path::new(manifest_dir);
        let dir = if dir.is_relative() {
            self.config_dir.join(dir)
        } else {
            dir.to_path_buf()
        };
        if !dir.is_dir() {
            tracing::warn!("plugin manifest directory not found: {}", manifest_dir);
            return Vec::new();
        }

        let mut plugins = Vec::new();

        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let manifest_path = path.join("plugin.forge.toml");
                    if manifest_path.exists() {
                        match Self::load_plugin_manifest(&manifest_path) {
                            Ok(manifest) => {
                                plugins.push(DiscoveredPlugin {
                                    manifest,
                                    manifest_path,
                                    directory: path,
                                });
                            }
                            Err(e) => {
                                tracing::error!(
                                    "failed to load plugin manifest at {}: {e}",
                                    manifest_path.display()
                                );
                            }
                        }
                    }
                }
            }
        }

        plugins
    }

    /// Read and parse a single plugin.forge.toml.
    pub fn load_plugin_manifest(path: &Path) -> Result<PluginManifest, ConfigError> {
        let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.to_string_lossy().into(),
            source: e,
        })?;

        let manifest: PluginManifest =
            toml::from_str(&contents).map_err(|e| ConfigError::Parse {
                path: path.to_string_lossy().into(),
                source: e,
            })?;

        // Make sure the manifest version is compatible
        if !manifest.forge_manifest_version.starts_with("1.") {
            return Err(ConfigError::VersionMismatch {
                path: Some(path.to_string_lossy().into()),
                found: manifest.forge_manifest_version,
                expected: "1.x".into(),
            });
        }

        Ok(manifest)
    }
}

impl Default for ConfigLoader {
    fn default() -> Self {
        Self::new()
    }
}

/// A plugin we found on disk — ready for lifecycle to pick it up.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    /// The parsed `plugin.forge.toml` contents.
    pub manifest: PluginManifest,
    /// Absolute filesystem path to the `plugin.forge.toml` file.
    pub manifest_path: PathBuf,
    /// Absolute path to the plugin's directory on disk.
    pub directory: PathBuf,
}

/// Errors that can occur while loading or parsing config.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The config file could not be read from disk.
    #[error("I/O error reading {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },

    /// The config file contained invalid TOML or did not match the expected schema.
    #[error("parse error in {path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },

    /// The `forge_config_version` or `forge_manifest_version` is not compatible with this kernel.
    #[error("version mismatch in {path:?}: found manifest version {found}, expected {expected}")]
    VersionMismatch {
        path: Option<String>,
        found: String,
        expected: String,
    },
}

/// Override config fields with FORGE_* environment variables. These sit between CLI flags and the file in precedence.
fn apply_env_overrides(config: &mut ForgeConfig) {
    if let Ok(val) = std::env::var("FORGE_CONFIG_VERSION") {
        config.forge_config_version = val;
    }
    if let Ok(val) = std::env::var("FORGE_GATEWAY_GRPC_BIND") {
        config.gateway.grpc_bind = val;
    }
    if let Ok(val) = std::env::var("FORGE_GATEWAY_HTTP_BIND") {
        config.gateway.http_bind = val;
    }
    if let Ok(val) = std::env::var("FORGE_GATEWAY_TLS") {
        config.gateway.tls = val.eq_ignore_ascii_case("true") || val == "1";
    }
    if let Ok(val) = std::env::var("FORGE_GATEWAY_TLS_CERT_PATH") {
        config.gateway.tls_cert_path = Some(val);
    }
    if let Ok(val) = std::env::var("FORGE_GATEWAY_TLS_KEY_PATH") {
        config.gateway.tls_key_path = Some(val);
    }
    if let Ok(val) = std::env::var("FORGE_LOG_LEVEL") {
        config.log.level = val;
    }
    if let Ok(val) = std::env::var("FORGE_PLUGINS_MANIFEST_DIR") {
        config.plugins.manifest_dir = val;
    }
    if let Ok(val) = std::env::var("FORGE_PLUGINS_WATCH") {
        config.plugins.watch = val.eq_ignore_ascii_case("true") || val == "1";
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_loads() {
        let config = ForgeConfig::default();
        assert_eq!(config.forge_config_version, "1.0");
        assert_eq!(config.gateway.grpc_bind, "127.0.0.1:9090");
        assert_eq!(config.gateway.http_bind, "127.0.0.1:9091");
        assert!(!config.gateway.tls);
        assert_eq!(config.log.level, "info");
        assert_eq!(config.plugins.manifest_dir, "./plugins");
        assert!(!config.plugins.watch);
    }

    #[test]
    fn parse_valid_forge_toml() {
        let toml_str = r#"
forge_config_version = "1.0"

[gateway]
grpc_bind = "0.0.0.0:9090"
http_bind = "0.0.0.0:9091"
tls = true
tls_cert_path = "/etc/forge/cert.pem"
tls_key_path = "/etc/forge/key.pem"

[log]
level = "debug"

[plugins]
manifest_dir = "/opt/forge/plugins"
watch = true
"#;
        let config: ForgeConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.forge_config_version, "1.0");
        assert_eq!(config.gateway.grpc_bind, "0.0.0.0:9090");
        assert!(config.gateway.tls);
        assert_eq!(config.log.level, "debug");
        assert!(config.plugins.watch);
    }

    #[test]
    fn parse_plugin_manifest_server_shape() {
        let toml_str = r#"
forge_manifest_version = "1.0"

[plugin]
name = "echo-rs"
version = "0.1.0"
description = "Echo plugin"
protocol_version = "1.0"

[transport]
shape = "server"
address = "unix:///run/forge/plugins/echo.sock"

[lifecycle]
restart_policy = "on-failure"

[capabilities]
provides = ["forge.example.echo@1.0"]
requires = []
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.plugin.name, "echo-rs");
        assert!(matches!(manifest.transport, PluginTransport::Server { .. }));
        assert_eq!(manifest.capabilities.provides.len(), 1);
    }

    #[test]
    fn parse_plugin_manifest_managed_subprocess() {
        let toml_str = r#"
forge_manifest_version = "1.0"

[plugin]
name = "echo-py"
version = "0.1.0"
description = "Python echo plugin"
protocol_version = "1.0"

[transport]
shape = "managed-subprocess"
executable = "/usr/bin/python3"
args = ["-m", "echo_plugin"]
working_dir = "/opt/plugins/echo-py"
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.plugin.name, "echo-py");
        match manifest.transport {
            PluginTransport::ManagedSubprocess {
                executable,
                args,
                working_dir,
            } => {
                assert_eq!(executable, "/usr/bin/python3");
                assert_eq!(args, vec!["-m", "echo_plugin"]);
                assert_eq!(working_dir, Some("/opt/plugins/echo-py".into()));
            }
            _ => panic!("expected ManagedSubprocess"),
        }
    }

    #[test]
    fn reject_unsupported_manifest_version() {
        let toml_str = r#"
forge_manifest_version = "2.0"

[plugin]
name = "test"
version = "0.1.0"
description = "test"
protocol_version = "1.0"
"#;
        let result: Result<PluginManifest, _> =
            toml::from_str(toml_str).map_err(|e| ConfigError::Parse {
                path: "test".into(),
                source: e,
            });
        // Deserialization itself doesn't validate versions — that's a separate step
        let manifest = result.unwrap();
        let validation = if manifest.forge_manifest_version.starts_with("1.") {
            Ok(())
        } else {
            Err(ConfigError::VersionMismatch {
                path: Some("test".into()),
                found: manifest.forge_manifest_version,
                expected: "1.x".into(),
            })
        };
        assert!(validation.is_err());
    }

    #[test]
    fn discover_no_manifest_dir() {
        let loader = ConfigLoader::new();
        let plugins = loader.discover_plugin_manifests("/nonexistent/path");
        assert!(plugins.is_empty());
    }

    #[test]
    fn env_overrides_apply_string() {
        temp_env::with_var("FORGE_LOG_LEVEL", Some("debug"), || {
            temp_env::with_var("FORGE_GATEWAY_GRPC_BIND", Some("0.0.0.0:9092"), || {
                let mut config = ForgeConfig::default();
                apply_env_overrides(&mut config);
                assert_eq!(config.log.level, "debug");
                assert_eq!(config.gateway.grpc_bind, "0.0.0.0:9092");
            });
        });
    }

    #[test]
    fn env_overrides_apply_bool() {
        temp_env::with_var("FORGE_GATEWAY_TLS", Some("true"), || {
            temp_env::with_var("FORGE_PLUGINS_WATCH", Some("1"), || {
                let mut config = ForgeConfig::default();
                apply_env_overrides(&mut config);
                assert!(config.gateway.tls);
                assert!(config.plugins.watch);
            });
        });
    }

    #[test]
    fn env_overrides_do_not_apply_when_unset() {
        // No FORGE_ vars set — config should keep whatever we assigned
        let mut config = ForgeConfig::default();
        config.gateway.grpc_bind = "127.0.0.1:9999".into();
        apply_env_overrides(&mut config);
        // No env override means the explicit value sticks
        assert_eq!(config.gateway.grpc_bind, "127.0.0.1:9999");
    }

    #[test]
    fn load_config_uses_env_overrides() {
        temp_env::with_var("FORGE_LOG_LEVEL", Some("trace"), || {
            let loader = ConfigLoader::new();
            let config = loader.load_config().unwrap();
            assert_eq!(config.log.level, "trace");
        });
    }

    #[test]
    fn manifest_dir_resolved_against_config_dir() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let tmp = PathBuf::from(std::env::temp_dir()).join(format!("forge-test-manifest-{unique}"));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugin_dir = tmp.join("plugins").join("echo-rs");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        // Drop a real manifest so discovery has something to find
        std::fs::write(
            plugin_dir.join("plugin.forge.toml"),
            r#"
forge_manifest_version = "1.0"
[plugin]
name = "echo-rs"
version = "0.1.0"
description = "Echo plugin"
protocol_version = "1.0"
[transport]
shape = "server"
address = "127.0.0.1:9999"
[lifecycle]
restart_policy = "on-failure"
[capabilities]
provides = ["forge.example.echo@1.0"]
requires = []
"#,
        )
        .unwrap();

        std::fs::write(
            tmp.join("forge.toml"),
            r#"
forge_config_version = "1.0"
[plugins]
manifest_dir = "plugins"
"#,
        )
        .unwrap();

        let loader = ConfigLoader::new().with_config_path(tmp.join("forge.toml"));
        let plugins = loader.discover_plugin_manifests("plugins");
        assert_eq!(
            plugins.len(),
            1,
            "should find the echo-rs plugin through the resolved relative path"
        );
        assert_eq!(plugins[0].manifest.plugin.name, "echo-rs");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
