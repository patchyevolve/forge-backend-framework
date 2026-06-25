use crate::bus::Bus;
use crate::config::{ConfigLoader, ForgeConfig};
use crate::registry::Registry;

/// Embed forge-backend in another program. Wraps the Registry + Bus so you get the full invocation pipeline without any gateway or gRPC listeners.
pub struct Kernel {
    registry: Registry,
    bus: Bus,
    _config: KernelConfig,
}

/// Tuning knobs for the embedded kernel. Start with defaults, tweak what you need.
#[derive(Default)]
pub struct KernelConfig {
    #[allow(dead_code)]
    config: ForgeConfig,
}

impl KernelConfig {
    /// Load settings from a forge.toml. Gateway-specific fields are quietly ignored in embedded mode.
    pub fn from_file(path: &str) -> Result<Self, anyhow::Error> {
        let loader = ConfigLoader::new().with_config_path(path);
        let config = loader.load_config()?;
        Ok(Self { config })
    }
}

impl Kernel {
    /// Fire up an embedded kernel with the config you've set up.
    #[must_use]
    pub fn start(config: KernelConfig) -> Self {
        let registry = Registry::new();
        let bus = Bus::new(registry.clone());
        Self {
            registry,
            bus,
            _config: config,
        }
    }

    /// Returns a reference to the kernel's capability registry.
    ///
    /// ```
    /// # use forge_backend::kernel::{Kernel, KernelConfig};
    /// let kernel = Kernel::start(KernelConfig::default());
    /// let _ = kernel.registry();
    /// ```
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Returns a reference to the kernel's message bus.
    ///
    /// ```
    /// # use forge_backend::kernel::{Kernel, KernelConfig};
    /// let kernel = Kernel::start(KernelConfig::default());
    /// let _ = kernel.bus();
    /// ```
    pub fn bus(&self) -> &Bus {
        &self.bus
    }
}
