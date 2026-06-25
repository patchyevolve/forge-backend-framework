use std::collections::HashMap;

use forge_plugin_sdk_rust::{Capability, InvokeContext, InvokeResult, Plugin, PluginError, PluginServer};
use serde::{Deserialize, Serialize};

// A plugin with a custom JSON schema — structured input and output
// instead of just echoing raw bytes.

#[derive(Deserialize)]
struct GreetRequest {
    name: String,
    title: Option<String>,
    language: Option<String>,
}

#[derive(Serialize)]
struct GreetResponse {
    greeting: String,
    polite: bool,
}

struct CustomCapabilityPlugin;

#[forge_plugin_sdk_rust::async_trait]
impl Plugin for CustomCapabilityPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new(
            "forge.example.greet",
            "1.0",
        )]
    }

    async fn health_check(&self) -> bool { true }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.greet" => {
                let req: GreetRequest = serde_json::from_slice(&ctx.payload)
                    .map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected GreetRequest JSON: {e}"),
                        details: HashMap::new(),
                    })?;

                let prefix = req.title.map(|t| format!("{t} ")).unwrap_or_default();
                let greeting = match req.language.as_deref() {
                    Some("es") => format!("¡Hola, {prefix}{}! ¿Cómo estás?", req.name),
                    Some("fr") => format!("Bonjour, {prefix}{} ! Comment allez-vous ?", req.name),
                    _ => format!("Hello, {prefix}{}! How are you?", req.name),
                };

                Ok(serde_json::to_vec(&GreetResponse {
                    greeting,
                    polite: true,
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
        .unwrap_or_else(|_| "127.0.0.1:50062".into());
    PluginServer::new(CustomCapabilityPlugin).serve_shape_a().await
}
