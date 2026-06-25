use std::collections::HashMap;
use std::sync::Mutex;

use forge_plugin_sdk_rust::{
    Capability, InvokeContext, InvokeResult, Plugin, PluginError, PluginServer,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct WriteRequest {
    key: String,
    value: String,
}

#[derive(Serialize)]
struct WriteResponse {
    stored: bool,
}

#[derive(Deserialize)]
struct QueryRequest {
    key: String,
}

#[derive(Serialize)]
struct QueryResponse {
    value: Option<String>,
}

struct IngestPlugin {
    data: Mutex<HashMap<String, String>>,
}

#[forge_plugin_sdk_rust::async_trait]
impl Plugin for IngestPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::new("forge.example.ingest.write", "1.0.0"),
            Capability::new("forge.example.ingest.query", "1.0.0"),
        ]
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.ingest.write" => {
                let req: WriteRequest =
                    serde_json::from_slice(&ctx.payload).map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected WriteRequest JSON: {e}"),
                        details: HashMap::new(),
                    })?;
                self.data.lock().unwrap().insert(req.key, req.value);
                Ok(serde_json::to_vec(&WriteResponse { stored: true }).unwrap())
            }
            "forge.example.ingest.query" => {
                let req: QueryRequest =
                    serde_json::from_slice(&ctx.payload).map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected QueryRequest JSON: {e}"),
                        details: HashMap::new(),
                    })?;
                let value = self.data.lock().unwrap().get(&req.key).cloned();
                Ok(serde_json::to_vec(&QueryResponse { value }).unwrap())
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
        std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:51051");
    }
    PluginServer::new(IngestPlugin {
        data: Mutex::new(HashMap::new()),
    })
    .serve_shape_a()
    .await
}
