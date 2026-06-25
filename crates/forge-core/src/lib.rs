//! Forge Core — the embedding API for the forge plugin runtime.
//!
//! This crate provides the `Registry`, `Bus`, and `Kernel` primitives that you
//! can embed into your own application to invoke plugin capabilities without
//! running the full forge gateway.

pub mod bus;
pub mod config;
pub mod kernel;
pub mod lifecycle;
pub mod registry;
