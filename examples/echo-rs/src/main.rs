use forge::{Capability, InvokeContext, InvokeResult, PluginError, PluginServer};

struct EchoPlugin;

#[forge::async_trait]
impl forge::Plugin for EchoPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("forge.example.echo", "1.0.0")]
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.echo" => {
                let text = String::from_utf8_lossy(&ctx.payload);
                Ok(text.to_uppercase().into_bytes())
            }
            other => Err(PluginError::not_found(format!(
                "unknown capability: {other}"
            ))),
        }
    }

    async fn health_check(&self) -> bool {
        true
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Make sure this matches the address in plugin.forge.toml
    // so the kernel knows where to find us.
    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        unsafe {
            std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:50051");
        }
    }

    PluginServer::new(EchoPlugin).serve_shape_a().await
}
