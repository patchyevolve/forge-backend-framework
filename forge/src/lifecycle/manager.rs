use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tonic::transport::{Channel, Endpoint};
use uuid::Uuid;

use crate::proto::forge_plugin_client::ForgePluginClient;
use crate::proto::{DrainRequest, HealthCheckRequest, RegisterRequest};

use crate::bus::{Bus, PluginConnection};
use crate::config::{DiscoveredPlugin, PluginTransport};
use crate::lifecycle::PluginState;
use crate::registry::{PluginHandle, Registry};

/// What the lifecycle manager tracks for each plugin.
struct ManagedPlugin {
    state: PluginState,
    health_failures: u32,
    channel: Option<Channel>,
    drain_grace_period_ms: u64,
    /// Saved manifest so we know how to restart
    discovered: Option<DiscoveredPlugin>,
    restart_attempts: u32,
    /// True when a restart is already queued — stops us from double-spawning
    restart_scheduled: bool,
    /// How many times the file-watcher has retried connecting to this plugin.
    /// Separate from restart_attempts (which counts post-registration crash cycles)
    /// because "never came up" and "came up then crashed" are genuinely different
    /// situations and conflating their counters would give wrong semantics.
    /// Reset to 0 when the plugin reaches Ready.
    watch_restart_attempts: u32,
    /// Handle to the managed subprocess child, if spawned via ManagedSubprocess.
    child: Option<tokio::process::Child>,
}

/// A "please restart this plugin" message sent through the restart channel.
/// We use a channel instead of calling start_one_impl directly to break the async type cycle — the health
/// check loop detects crashes and needs to restart, but restart calls start_one_impl again.
struct RestartRequest {
    discovered: DiscoveredPlugin,
}

/// The lifecycle Manager — walks plugins through DISCOVERED → READY and handles health checks, draining, and shutdown.
#[derive(Clone)]
pub struct Manager {
    registry: Registry,
    bus: Bus,
    plugins: Arc<Mutex<HashMap<String, ManagedPlugin>>>,
    /// Send end of the crash → restart channel. The receiver is a background task spawned in Manager::new().
    restart_tx: mpsc::UnboundedSender<RestartRequest>,
}

impl Manager {
    /// Create a new lifecycle manager.
    ///
    /// Spawns a background task that processes crash/restart requests with
    /// exponential backoff.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use forgecore_backend_framework_daemon::lifecycle::Manager;
    /// use forgecore_backend_framework_daemon::registry::Registry;
    /// use forgecore_backend_framework_daemon::bus::Bus;
    ///
    /// # async fn example() {
    /// let registry = Registry::new();
    /// let bus = Bus::new(registry.clone());
    /// let manager = Manager::new(registry, bus);
    /// # }
    /// ```
    #[must_use]
    pub fn new(registry: Registry, bus: Bus) -> Self {
        let plugins: Arc<Mutex<HashMap<String, ManagedPlugin>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (restart_tx, mut restart_rx) = mpsc::unbounded_channel::<RestartRequest>();
        let r_registry = registry.clone();
        let r_bus = bus.clone();
        let r_plugins = plugins.clone();

        // Background task that processes restart requests. It passes its own restart_tx forward so
        // the health-check loop of a restarted plugin can report crashes through the same channel.
        let r_restart_tx = restart_tx.clone();
        tokio::spawn(async move {
            while let Some(req) = restart_rx.recv().await {
                let plugin_name = req.discovered.manifest.plugin.name.clone();
                let delay = {
                    let map = r_plugins.lock().await;
                    map.get(&plugin_name)
                        .map(|p| {
                            exponential_backoff(
                                req.discovered.manifest.lifecycle.restart_backoff_initial_ms,
                                req.discovered.manifest.lifecycle.restart_backoff_max_ms,
                                p.restart_attempts,
                            )
                        })
                        .unwrap_or(1000)
                };

                tracing::info!("plugin {plugin_name}: restart in {delay}ms");
                tokio::time::sleep(Duration::from_millis(delay)).await;

                if let Err(e) = Self::start_one_impl(
                    req.discovered,
                    r_registry.clone(),
                    r_bus.clone(),
                    r_plugins.clone(),
                    r_restart_tx.clone(),
                )
                .await
                {
                    tracing::error!("plugin {plugin_name}: restart failed — {e}");
                } else {
                    tracing::info!("plugin {plugin_name}: restarted — READY");
                }
            }
        });

        Self {
            registry,
            bus,
            plugins,
            restart_tx,
        }
    }

    /// Add a new plugin or update an existing one (e.g. after restart). Validates state changes through the transition function.
    async fn insert_or_update_plugin(
        plugins: &Arc<Mutex<HashMap<String, ManagedPlugin>>>,
        name: &str,
        state: PluginState,
        channel: Option<Channel>,
        drain_grace_period_ms: u64,
        discovered: Option<DiscoveredPlugin>,
    ) {
        let mut map = plugins.lock().await;
        if let Some(existing) = map.get_mut(name) {
            // Validate the state change first
            if let Ok(new) = existing.state.transition(state) {
                existing.state = new;
            }
            existing.channel = channel;
            existing.drain_grace_period_ms = drain_grace_period_ms;
            if let Some(d) = discovered {
                existing.discovered = Some(d);
            }
            existing.health_failures = 0;
            existing.restart_scheduled = false;
            existing.watch_restart_attempts = 0;
        } else {
            map.insert(
                name.to_string(),
                ManagedPlugin {
                    state,
                    health_failures: 0,
                    channel,
                    drain_grace_period_ms,
                    discovered,
                    restart_attempts: 0,
                    restart_scheduled: false,
                    watch_restart_attempts: 0,
                    child: None,
                },
            );
        }
    }

    async fn set_state(
        plugins: &Arc<Mutex<HashMap<String, ManagedPlugin>>>,
        name: &str,
        target: PluginState,
    ) {
        let mut map = plugins.lock().await;
        if let Some(entry) = map.get_mut(name)
            && let Ok(()) = entry.state.transition(target).map(|s| entry.state = s)
        {
            tracing::debug!("plugin {name}: state → {target:?}");
        }
    }

    /// Fire up every discovered plugin in parallel.
    ///
    /// Each plugin is spawned as a separate tokio task and the method
    /// waits for all of them to finish. Start failures are logged but
    /// don't block other plugins.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use forgecore_backend_framework_daemon::lifecycle::Manager;
    /// use forgecore_backend_framework_daemon::config::DiscoveredPlugin;
    ///
    /// # async fn example(manager: Manager, plugins: Vec<DiscoveredPlugin>) {
    /// manager.start_all(plugins).await;
    /// # }
    /// ```
    pub async fn start_all(&self, discovered: Vec<DiscoveredPlugin>) {
        let mut handles = Vec::new();
        for plugin in discovered {
            let name = plugin.manifest.plugin.name.clone();
            let plugins = self.plugins.clone();
            let registry = self.registry.clone();
            let bus = self.bus.clone();
            let restart_tx = self.restart_tx.clone();

            // Clone for the retry path: the primary copy is moved into start_one_with_tx.
            let disc_for_retry = plugin.clone();

            handles.push(tokio::spawn(async move {
                if let Err(e) = Self::start_one_with_tx(
                    plugin,
                    registry,
                    bus,
                    plugins.clone(),
                    restart_tx.clone(),
                )
                .await
                {
                    tracing::error!(
                        "plugin {name}: failed to start — {e}; \
                         scheduling one coordinator retry with exponential backoff"
                    );
                    // One-shot retry via the restart channel. The coordinator applies
                    // exponential backoff. If this also fails, no further retries happen
                    // from start_all — the watcher handles recurring retries separately
                    // when watch=true.
                    let _ = restart_tx.send(RestartRequest {
                        discovered: disc_for_retry,
                    });
                } else {
                    tracing::info!("plugin {name}: READY — capabilities registered");
                }
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }
    }

    /// Small wrapper that hands restart_tx through to the real start implementation.
    async fn start_one_with_tx(
        discovered: DiscoveredPlugin,
        registry: Registry,
        bus: Bus,
        plugins: Arc<Mutex<HashMap<String, ManagedPlugin>>>,
        restart_tx: mpsc::UnboundedSender<RestartRequest>,
    ) -> anyhow::Result<()> {
        Self::start_one_impl(discovered, registry, bus, plugins, restart_tx).await
    }

    /// The actual start logic, parameterized over the restart channel so both initial start and restart use the same code.
    async fn start_one_impl(
        discovered: DiscoveredPlugin,
        registry: Registry,
        bus: Bus,
        plugins: Arc<Mutex<HashMap<String, ManagedPlugin>>>,
        restart_tx: mpsc::UnboundedSender<RestartRequest>,
    ) -> anyhow::Result<()> {
        let manifest = discovered.manifest.clone();
        let plugin_name = manifest.plugin.name.clone();
        let instance_id = Uuid::new_v4().to_string();
        let drain_grace = manifest.lifecycle.drain_grace_period_ms;

        // On restart the existing entry may be Stopped or Discovered — move to Discovered
        // if needed so the transition chain works.
        // insert_or_update_plugin below will then push it to Connecting.
        {
            let mut map = plugins.lock().await;
            if let Some(p) = map.get_mut(&plugin_name)
                && p.state != PluginState::Discovered
                && let Ok(new) = p.state.transition(PluginState::Discovered)
            {
                p.state = new;
            }
        }

        // Insert or update the entry — this sets state to Connecting and refreshes channel/manifest
        Self::insert_or_update_plugin(
            &plugins,
            &plugin_name,
            PluginState::Connecting,
            None,
            drain_grace,
            Some(discovered.clone()),
        )
        .await;

        // Figure out where to connect based on the transport shape, then dial in
        let channel = match &manifest.transport {
            PluginTransport::Server { address } => {
                let ep = Endpoint::new(address.clone())?
                    .connect_timeout(Duration::from_secs(10))
                    .timeout(Duration::from_secs(30));
                ep.connect().await?
            }
            PluginTransport::ManagedSubprocess {
                executable,
                args,
                working_dir,
            } => {
                // Kill any previous child from a failed retry attempt
                {
                    let mut map = plugins.lock().await;
                    if let Some(p) = map.get_mut(&plugin_name)
                        && let Some(mut child) = p.child.take()
                    {
                        tracing::warn!(
                            "plugin {plugin_name}: killing orphan child from previous attempt"
                        );
                        let _ = child.start_kill();
                        let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                    }
                }

                let sock = tokio::net::TcpSocket::new_v4()?;
                sock.set_reuseaddr(true)?;
                sock.bind("127.0.0.1:0".parse()?)?;
                let listener = sock.listen(1024)?;
                let addr = listener.local_addr()?;
                let listen_addr = format!("{}:{}", addr.ip(), addr.port());

                drop(listener);

                let mut cmd = tokio::process::Command::new(executable);
                cmd.args(args)
                    .env("FORGE_LISTEN_ADDR", &listen_addr)
                    .env("FORGE_CALLBACK_ADDR", &listen_addr)
                    .env("FORGE_PLUGIN_DIR", &discovered.directory);
                let work_dir = working_dir.as_ref().map(|rel| {
                    if std::path::Path::new(rel).is_relative() {
                        discovered.directory.join(rel)
                    } else {
                        std::path::PathBuf::from(rel)
                    }
                });
                if let Some(dir) = &work_dir {
                    cmd.current_dir(dir);
                }
                let child = cmd.spawn().map_err(|e| {
                    anyhow::anyhow!("failed to spawn plugin process {executable}: {e}")
                })?;

                // Store the child handle so we can kill it during shutdown
                {
                    let mut map = plugins.lock().await;
                    if let Some(p) = map.get_mut(&plugin_name) {
                        p.child = Some(child);
                    }
                }

                tokio::time::sleep(Duration::from_millis(200)).await;
                let ep = Endpoint::new(format!("http://{listen_addr}"))?
                    .connect_timeout(Duration::from_secs(10))
                    .timeout(Duration::from_secs(30));
                ep.connect().await?
            }
        };

        // Move to HANDSHAKING and stash the channel
        Self::set_state(&plugins, &plugin_name, PluginState::Handshaking).await;
        {
            let mut map = plugins.lock().await;
            if let Some(p) = map.get_mut(&plugin_name) {
                p.channel = Some(channel.clone());
                p.restart_attempts = 0;
                p.restart_scheduled = false;
            }
        }

        // Handshake: call the Register RPC
        let mut client = ForgePluginClient::new(channel.clone());
        let register_req = RegisterRequest {
            kernel_protocol_version: "1.0".into(),
            instance_id: Uuid::new_v4().to_string(),
        };
        let register_resp = client
            .register(register_req)
            .await
            .map_err(|e| anyhow::anyhow!("Register RPC failed: {e}"))?;
        let capabilities = register_resp.into_inner().capabilities;

        // Tell the bus about this connection so it can route invocations
        let plugin_handle = PluginHandle {
            plugin_name: plugin_name.clone(),
            instance_id: instance_id.clone(),
        };
        let conn = PluginConnection {
            handle: plugin_handle.clone(),
            channel: channel.clone(),
        };
        bus.register_connection(conn).await;

        // Advertise each capability in the registry
        for cap in &capabilities {
            let version = semver::Version::parse(&cap.version)
                .unwrap_or_else(|_| semver::Version::new(1, 0, 0));
            registry.register(cap.name.clone(), version, plugin_handle.clone());
        }

        // All set — mark it READY
        Self::set_state(&plugins, &plugin_name, PluginState::Ready).await;

        // Reset the watch-retry counter — the plugin is now reachable.
        {
            let mut map = plugins.lock().await;
            if let Some(p) = map.get_mut(&plugin_name) {
                p.watch_restart_attempts = 0;
            }
        }

        // Start a background health-check loop that also detects crashes. Restarts go through the
        // restart_tx channel instead of calling start_one_impl directly to avoid async type recursion.
        let health_interval = manifest.lifecycle.health_check_interval_ms;
        let health_threshold = manifest.lifecycle.health_check_failure_threshold;
        let restart_policy = manifest.lifecycle.restart_policy.clone();
        let hc_name = plugin_name.clone();
        let hc_plugins = plugins.clone();
        let hc_registry = registry.clone();
        let hc_bus = bus.clone();
        let hc_restart_tx = restart_tx.clone();

        tokio::spawn(async move {
            // Give the plugin one interval to settle before we start prodding it
            tokio::time::sleep(Duration::from_millis(health_interval)).await;
            let mut interval = tokio::time::interval(Duration::from_millis(health_interval));
            loop {
                interval.tick().await;

                let (current_state, current_channel) = {
                    let map = hc_plugins.lock().await;
                    let p = map.get(&hc_name);
                    (p.map(|p| p.state), p.and_then(|p| p.channel.clone()))
                };

                match current_state {
                    Some(PluginState::Ready) | Some(PluginState::Degraded) => {
                        let Some(ch) = current_channel else {
                            crash_and_schedule_restart(
                                &hc_name,
                                &hc_plugins,
                                &hc_registry,
                                &hc_bus,
                                &restart_policy,
                                &hc_restart_tx,
                            )
                            .await;
                            break;
                        };

                        let mut hc_client = ForgePluginClient::new(ch.clone());
                        let hc_result = tokio::time::timeout(
                            Duration::from_secs(5),
                            hc_client.health_check(HealthCheckRequest {}),
                        )
                        .await;
                        let hc_result = match hc_result {
                            Ok(r) => r,
                            Err(_) => {
                                // Timeout is treated as Unavailable (dead plugin)
                                crash_and_schedule_restart(
                                    &hc_name,
                                    &hc_plugins,
                                    &hc_registry,
                                    &hc_bus,
                                    &restart_policy,
                                    &hc_restart_tx,
                                )
                                .await;
                                break;
                            }
                        };
                        match hc_result {
                            Ok(resp) => {
                                if resp.into_inner().healthy {
                                    let mut map = hc_plugins.lock().await;
                                    if let Some(p) = map.get_mut(&hc_name) {
                                        p.health_failures = 0;
                                        if p.state == PluginState::Degraded
                                            && let Ok(()) = p
                                                .state
                                                .transition(PluginState::Ready)
                                                .map(|s| p.state = s)
                                        {
                                            tracing::info!(
                                                "{hc_name}: recovered — DEGRADED → READY"
                                            );
                                        }
                                    }
                                } else {
                                    let mut map = hc_plugins.lock().await;
                                    if let Some(p) = map.get_mut(&hc_name) {
                                        p.health_failures += 1;
                                        if p.health_failures >= health_threshold
                                            && p.state == PluginState::Ready
                                            && let Ok(()) = p
                                                .state
                                                .transition(PluginState::Degraded)
                                                .map(|s| p.state = s)
                                        {
                                            tracing::warn!(
                                                "{hc_name}: health check {}/{} failed — READY → DEGRADED",
                                                p.health_failures,
                                                health_threshold
                                            );
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                let is_dead = e.code() == tonic::Code::Unavailable;
                                if is_dead {
                                    crash_and_schedule_restart(
                                        &hc_name,
                                        &hc_plugins,
                                        &hc_registry,
                                        &hc_bus,
                                        &restart_policy,
                                        &hc_restart_tx,
                                    )
                                    .await;
                                    break;
                                } else {
                                    let mut map = hc_plugins.lock().await;
                                    if let Some(p) = map.get_mut(&hc_name) {
                                        p.health_failures += 1;
                                        if p.health_failures >= health_threshold
                                            && p.state == PluginState::Ready
                                            && let Ok(()) = p
                                                .state
                                                .transition(PluginState::Degraded)
                                                .map(|s| p.state = s)
                                        {
                                            tracing::warn!(
                                                "{hc_name}: health check {}/{} failed — READY → DEGRADED ({e})",
                                                p.health_failures,
                                                health_threshold
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Some(PluginState::Stopped) | None => break,
                    _ => {}
                }
            }
        });

        Ok(())
    }

    /// Call Drain RPC on one plugin and wait for it to finish shutting down.
    async fn drain_plugin_inner(
        map: &mut HashMap<String, ManagedPlugin>,
        name: &str,
        registry: &Registry,
        bus: &Bus,
    ) {
        let channel = map.get(name).and_then(|p| p.channel.clone());
        let grace = map
            .get(name)
            .map(|p| p.drain_grace_period_ms)
            .unwrap_or(10_000);

        if let Some(ch) = channel {
            let mut client = ForgePluginClient::new(ch);
            let _ = client
                .drain(DrainRequest {
                    grace_period_ms: grace as u32,
                })
                .await;
            tokio::time::sleep(Duration::from_millis(grace)).await;
        }

        // Kill the subprocess child if managed
        if let Some(p) = map.get_mut(name)
            && let Some(mut child) = p.child.take()
        {
            tracing::info!("plugin {name}: killing subprocess");
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
        }

        if let Some(p) = map.get_mut(name)
            && let Ok(()) = p
                .state
                .transition(PluginState::Stopped)
                .map(|s| p.state = s)
        {
            tracing::info!("plugin {name}: DRAINING → STOPPED");
        }

        registry.deregister(&PluginHandle {
            plugin_name: name.to_string(),
            instance_id: String::new(),
        });
        bus.remove_connection(&PluginHandle {
            plugin_name: name.to_string(),
            instance_id: String::new(),
        })
        .await;
    }

    /// Restart a plugin: drain it, then flip back to Discovered so the lifecycle picks it up again.
    pub async fn restart_plugin(&self, name: &str) {
        let mut map = self.plugins.lock().await;
        if let Some(p) = map.get(name)
            && p.state != PluginState::Ready
            && p.state != PluginState::Degraded
        {
            return;
        }
        if let Some(p) = map.get_mut(name)
            && let Ok(()) = p
                .state
                .transition(PluginState::Draining)
                .map(|s| p.state = s)
        {
            tracing::info!("plugin {name}: operator restart — → DRAINING");
        }
        drop(map);

        let mut map = self.plugins.lock().await;
        Self::drain_plugin_inner(&mut map, name, &self.registry, &self.bus).await;

        if let Some(p) = map.get_mut(name) {
            if let Ok(()) = p
                .state
                .transition(PluginState::Discovered)
                .map(|s| p.state = s)
            {
                tracing::info!("plugin {name}: STOPPED → DISCOVERED (ready for restart)");
            }
            p.restart_attempts = 0;
        }
    }

    /// Start a plugin if it's new or stuck in a non-Runnable state (Connecting, Stopped, or absent).
    /// If the plugin is already Ready/Degraded, drain and restart it instead.
    /// This is the method the file-watcher uses to handle both new manifests and manifest changes.
    ///
    /// **Safety:** this method assumes single-caller-at-a-time for a given plugin name.
    /// The file-watcher's poll loop is sequential and is the only caller today.
    /// Adding a second concurrent caller (e.g. a future CLI command) would create a
    /// race: two `start_one_impl` calls for the same plugin could double-dial the
    /// same gRPC address, both succeed, and leave duplicate connection state.
    pub async fn start_plugin_if_new(&self, discovered: DiscoveredPlugin) {
        let name = discovered.manifest.plugin.name.clone();
        let is_runnable = {
            let map = self.plugins.lock().await;
            map.get(&name)
                .map(|p| p.state == PluginState::Ready || p.state == PluginState::Degraded)
                .unwrap_or(false)
        };

        if is_runnable {
            self.restart_plugin(&name).await;
            // Drain sets the state to Discovered — now push through to Connecting + start.
            if let Err(e) = Self::start_one_impl(
                discovered,
                self.registry.clone(),
                self.bus.clone(),
                self.plugins.clone(),
                self.restart_tx.clone(),
            )
            .await
            {
                tracing::error!("plugin {name}: failed to restart — {e}");
            }
        } else {
            tracing::info!("plugin {name}: (re)starting — current state is not Ready/Degraded");
            if let Err(e) = Self::start_one_impl(
                discovered,
                self.registry.clone(),
                self.bus.clone(),
                self.plugins.clone(),
                self.restart_tx.clone(),
            )
            .await
            {
                tracing::error!("plugin {name}: failed to start — {e}");
            }
        }
    }

    /// Retry a plugin that's stuck in a non-Runnable state (Connecting/Stopped).
    ///
    /// Uses a per-plugin attempt counter (`watch_restart_attempts`) separate from
    /// the crash-restart `restart_attempts` — "never came up" and "came up then
    /// crashed" are different situations and conflating their counters would give
    /// wrong backoff semantics.
    ///
    /// The cap (`restart_max_attempts`) is read from the plugin's own manifest,
    /// same default as the crash-restart cap (5) for predictability.
    ///
    /// Once the cap is exhausted the plugin is transitioned to Stopped and a
    /// clear terminal log message is emitted so an operator can grep for it.
    /// Returns `true` if a retry was attempted, `false` if capped or absent.
    pub async fn retry_plugin_watch(&self, name: &str) -> bool {
        let disc = {
            let mut map = self.plugins.lock().await;
            let Some(p) = map.get_mut(name) else {
                return false;
            };
            if p.state == PluginState::Ready || p.state == PluginState::Degraded {
                return true;
            }
            let max = p
                .discovered
                .as_ref()
                .map(|d| d.manifest.lifecycle.restart_max_attempts)
                .unwrap_or(5);
            p.watch_restart_attempts += 1;
            let attempt = p.watch_restart_attempts;
            if attempt >= max {
                let addr = p
                    .discovered
                    .as_ref()
                    .map(|d| match &d.manifest.transport {
                        PluginTransport::Server { address } => address.clone(),
                        PluginTransport::ManagedSubprocess { executable, .. } => {
                            format!("subprocess:{executable}")
                        }
                    })
                    .unwrap_or_else(|| "unknown".into());
                p.state = p.state.transition(PluginState::Stopped).unwrap_or(p.state);
                tracing::error!(
                    "plugin {name}: exhausted {max} watch-retry attempts — giving up. \
                     The plugin at {addr} was never reachable. \
                     Fix the address or start the process, then remove and re-add \
                     the manifest so the watcher treats it as new."
                );
                return false;
            }
            tracing::info!(
                "plugin {name}: watch-retry {attempt}/{max} — current state {:?}",
                p.state
            );
            p.discovered.clone()
        };
        let Some(d) = disc else {
            tracing::error!("plugin {name}: cannot retry — no cached manifest (BUG)");
            return false;
        };
        if let Err(e) = Self::start_one_impl(
            d,
            self.registry.clone(),
            self.bus.clone(),
            self.plugins.clone(),
            self.restart_tx.clone(),
        )
        .await
        {
            tracing::error!("plugin {name}: watch-retry failed — {e}");
        }
        true
    }

    /// Gracefully shut down everything — calls Drain RPC on each plugin and waits.
    /// Will force-kill managed subprocesses after the drain grace period.
    pub async fn shutdown_all(&self) {
        let names = {
            let map = self.plugins.lock().await;
            map.keys().cloned().collect::<Vec<_>>()
        };

        // Phase 1: mark all plugins as Draining
        {
            let mut map = self.plugins.lock().await;
            for name in &names {
                if let Some(p) = map.get_mut(name)
                    && (p.state == PluginState::Ready || p.state == PluginState::Degraded)
                    && let Ok(()) = p
                        .state
                        .transition(PluginState::Draining)
                        .map(|s| p.state = s)
                {
                    tracing::info!("plugin {name}: shutdown — → DRAINING");
                }
            }
        }

        // Phase 2: drain RPC + wait for graceful shutdown
        let mut map = self.plugins.lock().await;
        for name in &names {
            Self::drain_plugin_inner(&mut map, name, &self.registry, &self.bus).await;
        }
        drop(map);

        // Phase 3: force-kill any remaining subprocess children
        {
            let mut map = self.plugins.lock().await;
            for name in &names {
                if let Some(p) = map.get_mut(name)
                    && let Some(mut child) = p.child.take()
                {
                    tracing::info!("plugin {name}: force-killing subprocess");
                    let _ = child.start_kill();
                    // Give it a moment, then reap
                    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                }
            }
        }
    }

    /// Check what state a plugin is in.
    #[must_use]
    pub async fn plugin_state(&self, name: &str) -> Option<PluginState> {
        let map = self.plugins.lock().await;
        map.get(name).map(|p| p.state)
    }

    /// Return every plugin we know about and where they are in the lifecycle.
    #[must_use]
    pub async fn list_plugin_states(&self) -> Vec<(String, PluginState)> {
        let map = self.plugins.lock().await;
        map.iter().map(|(k, v)| (k.clone(), v.state)).collect()
    }
}

/// Called by the health check loop when a plugin looks dead. Flips it to STOPPED, deregisters everything,
/// and sends a RestartRequest through the channel for the coordinator to handle.
async fn crash_and_schedule_restart(
    name: &str,
    plugins: &Arc<Mutex<HashMap<String, ManagedPlugin>>>,
    registry: &Registry,
    bus: &Bus,
    restart_policy: &str,
    restart_tx: &mpsc::UnboundedSender<RestartRequest>,
) {
    let discovered = {
        let mut map = plugins.lock().await;
        if let Some(p) = map.get_mut(name) {
            if (p.state == PluginState::Ready || p.state == PluginState::Degraded)
                && let Ok(()) = p
                    .state
                    .transition(PluginState::Stopped)
                    .map(|s| p.state = s)
            {
                tracing::warn!("plugin {name}: connection lost — → STOPPED");
            }
            p.restart_attempts += 1;
            let should = !p.restart_scheduled
                && (restart_policy == "always"
                    || (restart_policy == "on-failure"
                        && p.restart_attempts
                            <= p.discovered
                                .as_ref()
                                .map(|d| d.manifest.lifecycle.restart_max_attempts)
                                .unwrap_or(5)));
            if should {
                p.restart_scheduled = true;
            }
            if should { p.discovered.clone() } else { None }
        } else {
            None
        }
    };

    // Remove this plugin's entries from the registry and bus
    registry.deregister(&PluginHandle {
        plugin_name: name.to_string(),
        instance_id: String::new(),
    });
    bus.remove_connection(&PluginHandle {
        plugin_name: name.to_string(),
        instance_id: String::new(),
    })
    .await;

    // Fire the restart request off through the channel — fire and forget
    if let Some(d) = discovered {
        let _ = restart_tx.send(RestartRequest { discovered: d });
    }
}

/// Exponential backoff: start at initial_ms and double each attempt, but never go over max_ms.
fn exponential_backoff(initial_ms: u64, max_ms: u64, current_attempt: u32) -> u64 {
    if current_attempt <= 1 {
        return initial_ms;
    }
    let mut backoff = initial_ms;
    for _ in 1..current_attempt {
        backoff = backoff.saturating_mul(2).min(max_ms);
    }
    backoff
}
