use std::collections::HashMap;
use std::sync::Mutex;

use forge::{
    Capability, InvokeContext, InvokeResult, Plugin, PluginError, PluginServer,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct PutRequest {
    key: String,
    value: String,
}

#[derive(Serialize)]
struct PutResponse {
    stored: bool,
    total: usize,
}

struct StorePlugin {
    records: Mutex<Vec<(String, String)>>,
}

#[forge::async_trait]
impl Plugin for StorePlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("forge.example.store.put", "1.0.0")]
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.store.put" => {
                let req: PutRequest =
                    serde_json::from_slice(&ctx.payload).map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected PutRequest JSON: {e}"),
                        details: HashMap::new(),
                    })?;
                let mut records = self.records.lock().unwrap();
                records.push((req.key, req.value));
                Ok(serde_json::to_vec(&PutResponse {
                    stored: true,
                    total: records.len(),
                })
                .unwrap())
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
    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        unsafe { std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:51052"); }
    }
    PluginServer::new(StorePlugin {
        records: Mutex::new(Vec::new()),
    })
    .serve_shape_a()
    .await
}
