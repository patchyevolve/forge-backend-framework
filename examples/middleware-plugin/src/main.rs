use std::collections::HashMap;
use std::time::Instant;

use forge_plugin_sdk_rust::{
    Capability, InvokeContext, InvokeResult, KernelClient, Plugin, PluginError, PluginServer,
};

// A middleware plugin that wraps another capability with logging,
// timing, and error handling. It delegates to the real plugin
// through the kernel's gRPC gateway.

const UPSTREAM_CAP: &str = "forge.example.echo";
const KERNEL_GRPC_ADDR: &str = "http://127.0.0.1:50051";

struct MiddlewarePlugin;

#[forge_plugin_sdk_rust::async_trait]
impl Plugin for MiddlewarePlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("forge.example.echo.middleware", "1.0")]
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.echo.middleware" => {
                let start = Instant::now();
                tracing::info!(
                    "middleware: forwarding to {UPSTREAM_CAP} request_id={}",
                    ctx.request_id
                );

                // Open a connection to the kernel and invoke the upstream plugin
                let kernel =
                    KernelClient::connect(KERNEL_GRPC_ADDR)
                        .await
                        .map_err(|e| PluginError {
                            code: "KERNEL_CONNECT_FAILED".into(),
                            message: e.to_string(),
                            details: HashMap::new(),
                        })?;

                let result = kernel
                    .invoke(
                        UPSTREAM_CAP,
                        ctx.payload.to_vec(),
                        ctx.metadata.clone(),
                        &ctx.request_id,
                    )
                    .await;

                let elapsed = start.elapsed();
                match &result {
                    Ok(bytes) => {
                        tracing::info!(
                            "middleware: {UPSTREAM_CAP} returned {} bytes in {:?}",
                            bytes.len(),
                            elapsed
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "middleware: {UPSTREAM_CAP} failed: {} {} ({:?})",
                            e.code,
                            e.message,
                            elapsed
                        );
                    }
                }

                result
            }
            other => Err(PluginError::not_found(format!(
                "unknown capability: {other}"
            ))),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let _addr = std::env::var("FORGE_LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:50063".into());
    PluginServer::new(MiddlewarePlugin).serve_shape_a().await
}
