use std::collections::HashMap;

use forge_plugin_sdk_rust::{
    Capability, InvokeContext, InvokeResult, KernelClient, PluginError, PluginServer,
};

struct HttpRouterPlugin;

#[forge_plugin_sdk_rust::async_trait]
impl forge_plugin_sdk_rust::Plugin for HttpRouterPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("forge.http.route", "1.0.0")]
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.http.route" => {
                let text = String::from_utf8_lossy(&ctx.payload);
                let req: HashMap<String, serde_json::Value> =
                    serde_json::from_str(&text).map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected JSON with 'method', 'path', 'headers': {e}"),
                        details: HashMap::new(),
                    })?;

                let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
                let path = req.get("path").and_then(|v| v.as_str()).unwrap_or("/");
                let headers = req
                    .get("headers")
                    .and_then(|v| v.as_object())
                    .map(|m| {
                        m.iter()
                            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                            .collect::<HashMap<_, _>>()
                    })
                    .unwrap_or_default();

                tracing::info!("http-router: {method} {path} request_id={}", ctx.request_id);

                // Pull the kernel address out of the context — needed to make plugin-to-plugin calls
                let kernel_addr =
                    ctx.metadata
                        .get("kernel_grpc_addr")
                        .ok_or_else(|| PluginError {
                            code: "NO_KERNEL_ADDR".into(),
                            message: "kernel_grpc_addr not provided in metadata".into(),
                            details: HashMap::new(),
                        })?;

                // Open a gRPC connection to the kernel so we can invoke other plugins
                let kernel = KernelClient::connect(kernel_addr)
                    .await
                    .map_err(|e| PluginError {
                        code: "KERNEL_CONNECT_FAILED".into(),
                        message: e.to_string(),
                        details: HashMap::new(),
                    })?;

                // Auth first — if there's no Authorization header, deny by default
                let auth_header = match headers.get("authorization") {
                    Some(h) => h,
                    None => {
                        tracing::warn!("http-router: no auth header");
                        let error_resp = serde_json::json!({
                            "status": 401,
                            "body": "Authorization header required",
                        });
                        return Ok(serde_json::to_vec(&error_resp).unwrap());
                    }
                };
                {
                    let token = auth_header.strip_prefix("Bearer ").unwrap_or(auth_header);
                    let auth_payload = serde_json::json!({"token": token});
                    let auth_meta = HashMap::new();
                    let auth_result = kernel
                        .invoke(
                            "forge.auth.verify",
                            serde_json::to_vec(&auth_payload).unwrap(),
                            auth_meta,
                            &ctx.request_id,
                        )
                        .await;

                    match auth_result {
                        Ok(bytes) => {
                            let resp: serde_json::Value =
                                serde_json::from_slice(&bytes).map_err(|e| PluginError {
                                    code: "AUTH_RESPONSE_PARSE_ERROR".into(),
                                    message: e.to_string(),
                                    details: HashMap::new(),
                                })?;
                            if resp.get("valid").and_then(|v| v.as_bool()).unwrap_or(false) {
                                tracing::info!(
                                    "http-router: auth OK for {}",
                                    resp.get("sub").and_then(|v| v.as_str()).unwrap_or("?")
                                );
                            } else {
                                tracing::warn!("http-router: auth FAILED");
                                let error_resp = serde_json::json!({
                                    "status": 401,
                                    "body": "Unauthorized",
                                });
                                return Ok(serde_json::to_vec(&error_resp).unwrap());
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                "http-router: auth invoke failed: {} {}",
                                e.code,
                                e.message
                            );
                            let error_resp = serde_json::json!({
                                "status": 500,
                                "body": "Auth service unavailable",
                            });
                            return Ok(serde_json::to_vec(&error_resp).unwrap());
                        }
                    }
                }

                // Now route the request — map HTTP method+path to data queries
                let result = match (method, path) {
                    ("GET", "/users") | ("GET", "/users/") => {
                        let query_payload =
                            serde_json::json!({"sql": "SELECT id, name, email FROM users"});
                        kernel
                            .invoke(
                                "forge.data.query",
                                serde_json::to_vec(&query_payload).unwrap(),
                                HashMap::new(),
                                &ctx.request_id,
                            )
                            .await
                    }
                    ("POST", "/users") | ("POST", "/users/") => {
                        let body = req.get("body").and_then(|v| v.as_str()).unwrap_or("");
                        let new_user: HashMap<String, serde_json::Value> =
                            serde_json::from_str(body).unwrap_or_default();
                        let name = new_user
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let email = new_user
                            .get("email")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown@example.com");
                        let query_payload = serde_json::json!({
                            "sql": "INSERT INTO users (name, email) VALUES (?1, ?2)",
                            "params": [name, email]
                        });
                        kernel
                            .invoke(
                                "forge.data.write",
                                serde_json::to_vec(&query_payload).unwrap(),
                                HashMap::new(),
                                &ctx.request_id,
                            )
                            .await
                    }
                    _ => {
                        let error_resp = serde_json::json!({
                            "status": 404,
                            "body": format!("Not Found: {method} {path}"),
                        });
                        return Ok(serde_json::to_vec(&error_resp).unwrap());
                    }
                };

                match result {
                    Ok(data) => {
                        let data_val: serde_json::Value =
                            serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null);
                        let resp = serde_json::json!({
                            "status": 200,
                            "body": data_val,
                        });
                        Ok(serde_json::to_vec(&resp).unwrap())
                    }
                    Err(e) => {
                        let error_resp = serde_json::json!({
                            "status": 500,
                            "body": format!("Data query failed: {} {}", e.code, e.message),
                        });
                        Ok(serde_json::to_vec(&error_resp).unwrap())
                    }
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

    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:50054");
    }

    PluginServer::new(HttpRouterPlugin).serve_shape_a().await
}
