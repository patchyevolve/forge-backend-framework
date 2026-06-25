use std::collections::HashMap;

use forge_plugin_sdk_rust::{
    Capability, InvokeContext, InvokeResult, KernelClient, Plugin, PluginError, PluginServer,
};
use serde::{Deserialize, Serialize};

// This plugin depends on two other capabilities.
// It queries the ingest plugin's data and pushes the result to the store plugin,
// chaining them both through the kernel.

const KERNEL_GRPC: &str = "http://127.0.0.1:9090";

#[derive(Deserialize)]
struct ProcessRequest {
    key: String,
    transform: String,
}

#[derive(Serialize)]
struct ProcessResponse {
    original: String,
    transformed: String,
    stored: bool,
}

struct ProcessPlugin;

#[forge_plugin_sdk_rust::async_trait]
impl Plugin for ProcessPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("forge.example.process", "1.0.0")]
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.process" => {
                let req: ProcessRequest =
                    serde_json::from_slice(&ctx.payload).map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected ProcessRequest JSON: {e}"),
                        details: HashMap::new(),
                    })?;

                let kernel = KernelClient::connect(KERNEL_GRPC)
                    .await
                    .map_err(|e| PluginError {
                        code: "KERNEL_CONNECT_FAILED".into(),
                        message: e.to_string(),
                        details: HashMap::new(),
                    })?;

                // Step 1: query the ingest plugin for the raw data
                let ingest_payload = serde_json::json!({ "key": req.key })
                    .to_string()
                    .into_bytes();
                let raw = kernel
                    .invoke(
                        "forge.example.ingest.query",
                        ingest_payload,
                        ctx.metadata.clone(),
                        &ctx.request_id,
                    )
                    .await?;

                let response: serde_json::Value =
                    serde_json::from_slice(&raw).map_err(|e| PluginError {
                        code: "INVALID_INGEST_RESPONSE".into(),
                        message: e.to_string(),
                        details: HashMap::new(),
                    })?;
                let original = response["value"].as_str().unwrap_or("").to_string();

                // Step 2: apply the requested transform
                let transformed = match req.transform.as_str() {
                    "uppercase" => original.to_uppercase(),
                    "reverse" => original.chars().rev().collect(),
                    _ => original.clone(),
                };

                // Step 3: store the transformed result
                let store_payload = serde_json::json!({
                    "key": req.key,
                    "value": transformed,
                })
                .to_string()
                .into_bytes();

                let store_result = kernel
                    .invoke(
                        "forge.example.store.put",
                        store_payload,
                        ctx.metadata.clone(),
                        &ctx.request_id,
                    )
                    .await?;

                let store_json: serde_json::Value = serde_json::from_slice(&store_result)
                    .unwrap_or(serde_json::json!({"stored": false}));
                let stored = store_json["stored"].as_bool().unwrap_or(false);

                Ok(serde_json::to_vec(&ProcessResponse {
                    original,
                    transformed,
                    stored,
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
        std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:51053");
    }
    PluginServer::new(ProcessPlugin).serve_shape_a().await
}
