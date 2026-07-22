//! # Forge Gateway
//!
//! HTTP + gRPC ingress layer. Translates external requests into plugin
//! invocations through the `Bus`.
//!
//! ## HTTP
//!
//! Route requests to capabilities based on `forge.toml` routing rules.
//! Supports auth middleware, rate limiting, CORS, static file serving,
//! and TLS.
//!
//! ## gRPC
//!
//! Exposes a gRPC gateway so that plugins (and external clients) can call
//! any capability through the kernel.
//!
//! ## Auth hooks
//!
//! Routes may specify `auth = "capability@version"`. Before dispatching the
//! request, the gateway invokes that capability to verify the caller. If it
//! returns `{ valid: false }` the request is rejected with 401.
//!
//! ## Example route configuration
//!
//! ```toml
//! [[gateway.routes]]
//! method = "POST"
//! path = "/login"
//! capability = "app.auth.login@1.0"
//!
//! [[gateway.routes]]
//! method = "GET"
//! path = "/alerts"
//! capability = "app.alerts@1.0"
//! auth = "app.auth.verify@1.0"
//! ```

mod grpc;
mod http;

pub use grpc::GrpcGateway;
pub use http::HttpGateway;

use crate::bus::Bus;
use crate::config::ForgeConfig;
use crate::lifecycle::Manager;
use crate::registry::Registry;

/// Holds both gRPC and HTTP listeners. Just a thin translation layer, no business logic.
pub struct Gateway {
    grpc: GrpcGateway,
    http: HttpGateway,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl Gateway {
    /// Build the gateway from its four dependencies.
    pub fn new(config: ForgeConfig, registry: Registry, bus: Bus, manager: Manager) -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let grpc = GrpcGateway::new(
            config.gateway.grpc_bind.clone(),
            config.gateway.tls,
            config.gateway.tls_cert_path.clone(),
            config.gateway.tls_key_path.clone(),
            bus.clone(),
            shutdown_rx.clone(),
        );

        let kernel_grpc_addr = format!("http://{}", config.gateway.grpc_bind);

        let http = HttpGateway::new(
            config.gateway.http_bind.clone(),
            config.gateway.tls,
            config.gateway.tls_cert_path.clone(),
            config.gateway.tls_key_path.clone(),
            registry,
            bus,
            manager,
            shutdown_rx,
            kernel_grpc_addr,
            config.gateway.static_dir.clone(),
            config.gateway.cors_allowed_origins,
            config.gateway.rate_limit_per_minute,
            config.gateway.max_body_size,
            config.gateway.routes,
        );

        Self {
            grpc,
            http,
            shutdown_tx,
        }
    }

    /// Fire up both listeners in parallel.
    /// Returns the first error from either gateway — if one fails the other is still shut down.
    pub async fn start(self) -> anyhow::Result<()> {
        let (grpc_res, http_res) = tokio::join!(
            tokio::spawn(self.grpc.serve()),
            tokio::spawn(self.http.serve()),
        );

        let grpc_err = grpc_res.ok().and_then(|r| r.err());
        let http_err = http_res.ok().and_then(|r| r.err());

        if let Some(e) = &grpc_err {
            tracing::error!("gRPC gateway error: {e}");
        }
        if let Some(e) = &http_err {
            tracing::error!("HTTP gateway error: {e}");
        }

        grpc_err
            .map(Err)
            .unwrap_or(Ok(()))
            .or_else(|_| http_err.map(Err).unwrap_or(Ok(())))
    }

    /// Signal both listeners to stop. Gracefully drains in-flight requests before returning.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}
