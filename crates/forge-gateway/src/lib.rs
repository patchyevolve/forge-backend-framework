mod grpc;
mod http;

pub use grpc::GrpcGateway;
pub use http::HttpGateway;

use forge_backend::bus::Bus;
use forge_backend::config::ForgeConfig;
use forge_backend::lifecycle::Manager;
use forge_backend::registry::Registry;

/// Holds both gRPC and HTTP listeners. Just a thin translation layer, no business logic.
pub struct Gateway {
    grpc: GrpcGateway,
    http: HttpGateway,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl Gateway {
    /// Build the gateway from its four dependencies. You'll need a [`ForgeConfig`] for
    /// bind addresses, a [`Registry`] for capability lookups, a [`Bus`] for dispatching
    /// invocations, and a [`Manager`] for lifecycle control.
    pub fn new(config: ForgeConfig, registry: Registry, bus: Bus, manager: Manager) -> Self {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let grpc = GrpcGateway::new(
            config.gateway.grpc_bind.clone(),
            bus.clone(),
            shutdown_rx.clone(),
        );

        let kernel_grpc_addr = format!("http://{}", config.gateway.grpc_bind);

        let http = HttpGateway::new(
            config.gateway.http_bind.clone(),
            config.gateway.tls,
            registry,
            bus,
            manager,
            shutdown_rx,
            kernel_grpc_addr,
        );

        Self {
            grpc,
            http,
            shutdown_tx,
        }
    }

    /// Fire up both listeners in parallel.
    pub async fn start(self) -> anyhow::Result<()> {
        let (grpc_res, http_res) = tokio::join!(
            tokio::spawn(self.grpc.serve()),
            tokio::spawn(self.http.serve()),
        );

        if let Err(e) = grpc_res? {
            tracing::error!("gRPC gateway error: {e}");
        }
        if let Err(e) = http_res? {
            tracing::error!("HTTP gateway error: {e}");
        }

        Ok(())
    }

    /// Signal both listeners to stop. Gracefully drains in-flight requests before returning.
    ///
    /// ```ignore
    /// gateway.shutdown();
    /// ```
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}
