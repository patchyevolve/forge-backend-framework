//! # Forge
//!
//! A backend operating environment. Orchestrates plugin processes,
//! routes HTTP/gRPC requests to capabilities, manages lifecycle.
//!
//! ## Feature flags
//!
//! - `default` — gateway + sdk (full framework)
//! - `gateway` — HTTP + gRPC listeners (axum, tonic)
//! - `sdk` — plugin SDK traits and types for writing plugins
//!
//! ## Example (embedding)
//!
//! ```no_run
//! use forge::kernel::{Kernel, KernelConfig};
//!
//! let kernel = Kernel::start(KernelConfig::from_file("forge.toml").unwrap());
//! let _registry = kernel.registry();
//! let _bus = kernel.bus();
//! ```

pub mod bus;
pub mod config;
pub mod kernel;
pub mod lifecycle;
pub mod proto;
pub mod registry;

#[cfg(feature = "gateway")]
pub mod gateway;

#[cfg(feature = "sdk")]
pub mod sdk;

#[cfg(feature = "sdk")]
pub use sdk::{async_trait, Capability, InvokeContext, InvokeResult, KernelClient, Plugin, PluginError, PluginServer};
