use std::collections::HashMap;

use forge::{Capability, InvokeContext, InvokeResult, PluginError, PluginServer};

struct AuthJwtPlugin;

#[forge::async_trait]
impl forge::Plugin for AuthJwtPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("forge.auth.verify", "1.0.0")]
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.auth.verify" => {
                let text = String::from_utf8_lossy(&ctx.payload);
                let req: HashMap<String, String> =
                    serde_json::from_str(&text).map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected JSON object with 'token' field: {e}"),
                        details: HashMap::new(),
                    })?;
                let token = req.get("token").ok_or_else(|| PluginError {
                    code: "MISSING_TOKEN".into(),
                    message: "payload must include 'token' field".into(),
                    details: HashMap::new(),
                })?;

                // Demo mode — just check the token matches the shared secret.
                // Swap this out for real JWT signature + expiry checks in production.
                let secret = std::env::var("FORGE_AUTH_SECRET")
                    .unwrap_or_else(|_| "forge-demo-secret".into());

                if token == &secret {
                    let resp = serde_json::json!({
                        "valid": true,
                        "sub": "demo-user",
                    });
                    Ok(serde_json::to_vec(&resp).unwrap())
                } else {
                    let resp = serde_json::json!({
                        "valid": false,
                        "error": "invalid token",
                    });
                    Ok(serde_json::to_vec(&resp).unwrap())
                }
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

    // Pick up the default addr from the manifest if nothing's set
    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:50052");
    }

    PluginServer::new(AuthJwtPlugin).serve_shape_a().await
}
