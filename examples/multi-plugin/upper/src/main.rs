use forge_plugin_sdk_rust::{Capability, InvokeContext, InvokeResult, Plugin, PluginError, PluginServer};

struct UpperPlugin;

#[forge_plugin_sdk_rust::async_trait]
impl Plugin for UpperPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new(
            "forge.example.text.upper",
            "1.0",
        )]
    }

    async fn health_check(&self) -> bool { true }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.text.upper" => {
                let text = String::from_utf8_lossy(&ctx.payload);
                Ok(text.to_uppercase().into_bytes())
            }
            other => Err(PluginError::not_found(format!("unknown capability: {other}"))),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into()))
        .init();

    let _addr = std::env::var("FORGE_LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:50064".into());
    PluginServer::new(UpperPlugin).serve_shape_a().await
}
