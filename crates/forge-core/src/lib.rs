//! # Forge Core
//!
//! The embedding API for the Forge plugin runtime. Use these primitives to
//! invoke plugin capabilities without running the full Forge gateway.
//!
//! ## Architecture
//!
//! - **`Registry`** — stores plugin manifests (name, version, capabilities).
//! - **`Bus`** — dispatches invocations to the right plugin process via gRPC.
//! - **`Kernel`** — ties Registry + Bus together, handles startup sequencing.
//! - **`Manager`** — spawns plugin subprocesses, health-checks, restarts.
//! - **`ConfigLoader`** — reads and validates `forge.toml`.
//!
//! ## Example
//!
//! ```no_run
//! use forge_backend::kernel::{Kernel, KernelConfig};
//!
//! let kernel = Kernel::start(KernelConfig::from_file("forge.toml").unwrap());
//! let _registry = kernel.registry();
//! let _bus = kernel.bus();
//! ```
//!
//! Typically you'd use the `forge` CLI binary instead of embedding directly.
//! This crate exists for advanced use cases where you want fine-grained
//! control over the plugin lifecycle.

pub mod bus;
pub mod config;
pub mod kernel;
pub mod lifecycle;
pub mod registry;
