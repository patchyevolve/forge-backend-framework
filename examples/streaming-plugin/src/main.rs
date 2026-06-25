use std::collections::HashMap;

use forge_plugin_sdk_rust::{Capability, InvokeContext, InvokeResult, Plugin, PluginError, PluginServer};
use serde::{Deserialize, Serialize};

// A plugin that paginates results instead of sending everything at once.
// Each invocation gets one page; the caller tracks offset/count.

#[derive(Deserialize)]
struct PageRequest {
    offset: usize,
    limit: usize,
}

#[derive(Serialize)]
struct PageResponse {
    items: Vec<String>,
    total: usize,
    next_offset: Option<usize>,
}

struct StreamingPlugin;

#[forge_plugin_sdk_rust::async_trait]
impl Plugin for StreamingPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new(
            "forge.example.stream",
            "1.0",
        )]
    }

    async fn health_check(&self) -> bool { true }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.stream" => {
                let req: PageRequest = serde_json::from_slice(&ctx.payload)
                    .map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected PageRequest JSON: {e}"),
                        details: HashMap::new(),
                    })?;

                let page: Vec<String> = (req.offset..req.offset + req.limit)
                    .map(|i| format!("item-{i}"))
                    .collect();

                let total = 10_000usize;
                let next = (req.offset + req.limit < total).then_some(req.offset + req.limit);

                Ok(serde_json::to_vec(&PageResponse {
                    items: page,
                    total,
                    next_offset: next,
                }).unwrap())
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
        .unwrap_or_else(|_| "127.0.0.1:50061".into());
    PluginServer::new(StreamingPlugin).serve_shape_a().await
}
