//! This crate has been renamed to `forgecore-backend-framework-daemon`.
//!
//! Migrate your `Cargo.toml`:
//!
//! ```toml
//! # Old (no longer published)
//! # forge-proto = "1.0"
//!
//! # New
//! forge = { package = "forgecore-backend-framework-daemon", version = "1.0" }
//! ```
//!
//! Your Rust source code continues to use `use forge::proto::...` unchanged.

pub use forgecore_backend_framework_daemon::*;
