use std::sync::atomic::{AtomicUsize, Ordering};

use forge::{Capability, InvokeContext, InvokeResult, Plugin, PluginServer};

struct CountPlugin {
    counter: AtomicUsize,
}

#[forge::async_trait]
impl Plugin for CountPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("forge.example.count", "1.0.0")]
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn invoke(&self, _ctx: InvokeContext) -> InvokeResult {
        let count = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(format!("invocation #{count}").into_bytes())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        unsafe {
            std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:51054");
        }
    }
    PluginServer::new(CountPlugin {
        counter: AtomicUsize::new(0),
    })
    .serve_shape_a()
    .await
}
