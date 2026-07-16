use std::collections::HashMap;

use forge::{
    Capability, InvokeContext, InvokeResult, KernelClient, Plugin, PluginError, PluginServer,
};

// This plugin depends on an upstream capability (uppercase) and chains
// its own processing after it. The dependency is declared in plugin.forge.toml
// so the kernel can verify the graph at startup.

const KERNEL_GRPC_ADDR: &str = "http://127.0.0.1:50051";

struct PipelinePlugin;

#[forge::async_trait]
impl Plugin for PipelinePlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("forge.example.text.pipeline", "1.0")]
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.text.pipeline" => {
                // Step 1: send to the upstream uppercase plugin
                let kernel =
                    KernelClient::connect(KERNEL_GRPC_ADDR)
                        .await
                        .map_err(|e| PluginError {
                            code: "KERNEL_CONNECT_FAILED".into(),
                            message: e.to_string(),
                            details: HashMap::new(),
                        })?;

                let uppercased = kernel
                    .invoke(
                        "forge.example.text.upper",
                        ctx.payload.to_vec(),
                        ctx.metadata.clone(),
                        &ctx.request_id,
                    )
                    .await?;

                // Step 2: append punctuation
                let mut result = String::from_utf8_lossy(&uppercased).to_string();
                result.push_str("!!!");

                tracing::info!(
                    "pipeline: request_id={} → uppercased + '!!!'",
                    ctx.request_id
                );

                Ok(result.into_bytes())
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

    let _addr = std::env::var("FORGE_LISTEN_ADDR").unwrap_or_else(|_| "127.0.0.1:50065".into());
    PluginServer::new(PipelinePlugin).serve_shape_a().await
}
