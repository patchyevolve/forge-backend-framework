use dashmap::DashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// An opaque handle for a plugin instance — the bus uses this to route invocations to the right connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginHandle {
    /// Unique instance identifier, set when the plugin starts.
    pub instance_id: String,
    /// Name of the plugin, used for routing and display.
    pub plugin_name: String,
}

/// What you get back when listing registered capabilities.
#[derive(Debug, Clone)]
pub struct CapabilitySummary {
    /// Fully-qualified capability name (e.g. "forge.example.echo").
    pub name: String,
    /// Semver version this plugin registered for this capability.
    pub version: semver::Version,
    /// Name of the plugin that registered this capability.
    pub plugin_name: String,
    /// Instance ID of the plugin providing this capability.
    pub plugin_instance_id: String,
}

/// One registered capability in the registry.
#[derive(Debug, Clone)]
struct CapabilityEntry {
    name: String,
    version: semver::Version,
    plugin_handle: PluginHandle,
}

/// The capability registry: a concurrent map from capability name to plugin handle + metadata.
#[derive(Debug, Clone)]
pub struct Registry {
    inner: Arc<DashMap<String, Vec<CapabilityEntry>>>,
    resolution: ResolutionStrategy,
    rr_counter: Arc<DashMap<String, AtomicUsize>>,
}

/// What to do when more than one plugin offers the same capability. First-ready-wins is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ResolutionStrategy {
    /// Use the first plugin that registered the capability.
    #[default]
    FirstReadyWins,
    /// Distribute invocations across matching plugins.
    RoundRobin,
}

impl Registry {
    /// Create an empty registry with the default [`ResolutionStrategy::FirstReadyWins`].
    ///
    /// ```
    /// use forgecore_backend_framework_daemon::registry::Registry;
    /// let reg = Registry::new();
    /// assert!(reg.list_capabilities().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            resolution: ResolutionStrategy::default(),
            rr_counter: Arc::new(DashMap::new()),
        }
    }

    /// Create an empty registry with a specific resolution strategy.
    ///
    /// ```
    /// use forgecore_backend_framework_daemon::registry::{Registry, ResolutionStrategy};
    /// let reg = Registry::with_resolution(ResolutionStrategy::RoundRobin);
    /// assert!(reg.list_capabilities().is_empty());
    /// ```
    #[must_use]
    pub fn with_resolution(strategy: ResolutionStrategy) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            resolution: strategy,
            rr_counter: Arc::new(DashMap::new()),
        }
    }

    /// Register a capability for a plugin. Only lifecycle calls this, when a plugin transitions into READY.
    pub fn register(&self, name: String, version: semver::Version, plugin_handle: PluginHandle) {
        let entry_name = name.clone();
        self.inner.entry(name).or_default().push(CapabilityEntry {
            name: entry_name,
            version,
            plugin_handle,
        });
    }

    /// Remove every capability this plugin handle registered. Called when the plugin goes into STOPPED.
    pub fn deregister(&self, plugin_handle: &PluginHandle) {
        self.inner.retain(|_key, entries| {
            entries.retain(|e| e.plugin_handle != *plugin_handle);
            !entries.is_empty()
        });
    }

    /// Find a plugin that provides a capability at a compatible version. Returns None if nothing matches.
    #[must_use]
    pub fn lookup(
        &self,
        capability: &str,
        version_constraint: &semver::VersionReq,
    ) -> Option<PluginHandle> {
        let entries = self.inner.get(capability)?;
        let matching: Vec<_> = entries
            .iter()
            .filter(|e| version_constraint.matches(&e.version))
            .collect();

        if matching.is_empty() {
            return None;
        }

        match self.resolution {
            ResolutionStrategy::FirstReadyWins => matching.first().map(|e| e.plugin_handle.clone()),
            ResolutionStrategy::RoundRobin => {
                let counter = self
                    .rr_counter
                    .entry(capability.to_string())
                    .or_insert(AtomicUsize::new(0));
                let idx = counter.fetch_add(1, Ordering::Relaxed) % matching.len();
                matching[idx].plugin_handle.clone().into()
            }
        }
    }

    /// List everything registered. Read-only — powers forge status.
    #[must_use]
    pub fn list_capabilities(&self) -> Vec<CapabilitySummary> {
        self.inner
            .iter()
            .flat_map(|entry| {
                let caps: Vec<CapabilitySummary> = entry
                    .value()
                    .iter()
                    .map(|cap| CapabilitySummary {
                        name: cap.name.clone(),
                        version: cap.version.clone(),
                        plugin_name: cap.plugin_handle.plugin_name.clone(),
                        plugin_instance_id: cap.plugin_handle.instance_id.clone(),
                    })
                    .collect();
                caps
            })
            .collect()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(name: &str, id: &str) -> PluginHandle {
        PluginHandle {
            plugin_name: name.to_string(),
            instance_id: id.to_string(),
        }
    }

    #[test]
    fn register_and_lookup() {
        let reg = Registry::new();
        let h = handle("test-plugin", "inst-1");
        reg.register(
            "forge.example.echo".into(),
            semver::Version::new(1, 0, 0),
            h.clone(),
        );

        let req = semver::VersionReq::parse("^1.0").unwrap();
        let result = reg.lookup("forge.example.echo", &req);
        assert_eq!(result, Some(h));
    }

    #[test]
    fn deregister_plugin() {
        let reg = Registry::new();
        let h = handle("test-plugin", "inst-1");
        reg.register(
            "forge.example.echo".into(),
            semver::Version::new(1, 0, 0),
            h.clone(),
        );
        reg.deregister(&h);

        let req = semver::VersionReq::parse("^1.0").unwrap();
        assert_eq!(reg.lookup("forge.example.echo", &req), None);
    }

    #[test]
    fn list_capabilities() {
        let reg = Registry::new();
        let h = handle("p1", "a");
        reg.register("cap.a".into(), semver::Version::new(1, 0, 0), h);
        reg.register(
            "cap.b".into(),
            semver::Version::new(2, 0, 0),
            handle("p1", "a"),
        );

        let list = reg.list_capabilities();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn version_mismatch_returns_none() {
        let reg = Registry::new();
        let h = handle("test-plugin", "inst-1");
        reg.register(
            "forge.example.echo".into(),
            semver::Version::new(2, 0, 0),
            h,
        );

        let req = semver::VersionReq::parse("^1.0").unwrap();
        assert_eq!(reg.lookup("forge.example.echo", &req), None);
    }

    #[test]
    fn lookup_nonexistent_capability() {
        let reg = Registry::new();
        let req = semver::VersionReq::parse("^1.0").unwrap();
        assert_eq!(reg.lookup("does.not.exist", &req), None);
    }
}
