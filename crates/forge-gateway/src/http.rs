use std::time::Duration;

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use forge_core::bus::{Bus, Invocation, InvocationError};
use forge_core::lifecycle::Manager;
use forge_core::registry::Registry;

/// HTTP (REST) gateway — exposes health, status, invoke, and plugin-management
/// endpoints. Thin wrapper around an axum server, no business logic.
pub struct HttpGateway {
    bind: String,
    _tls: bool,
    registry: Registry,
    bus: Bus,
    manager: Manager,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    kernel_grpc_addr: String,
}

impl HttpGateway {
    /// Build the HTTP gateway. You'll need a bind address, a TLS flag, a [`Registry`],
    /// a [`Bus`], a [`Manager`], a shutdown receiver, and the kernel gRPC address so
    /// the gateway can tell plugins how to call each other.
    pub fn new(
        bind: String,
        tls: bool,
        registry: Registry,
        bus: Bus,
        manager: Manager,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
        kernel_grpc_addr: String,
    ) -> Self {
        Self {
            bind,
            _tls: tls,
            registry,
            bus,
            manager,
            shutdown_rx,
            kernel_grpc_addr,
        }
    }

    /// Start the HTTP server. Blocks until the shutdown signal is received, then
    /// drains in-flight requests gracefully.
    pub async fn serve(self) -> anyhow::Result<()> {
        let addr: std::net::SocketAddr = self.bind.parse()?;

        let app_state = AppState {
            registry: self.registry,
            bus: self.bus,
            manager: self.manager,
            kernel_grpc_addr: self.kernel_grpc_addr,
        };

        let app = Router::new()
            .route("/healthz", get(healthz))
            .route("/v1/status", get(status))
            .route("/v1/invoke", post(invoke))
            .route("/v1/plugins/{name}/restart", post(plugin_restart))
            .with_state(app_state);

        tracing::info!(
            "HTTP gateway listening on {} (TLS: {})",
            addr,
            if self._tls {
                "enabled"
            } else {
                "disabled — local dev only"
            }
        );

        let listener = tokio::net::TcpListener::bind(addr).await?;

        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let mut rx = self.shutdown_rx;
                loop {
                    if *rx.borrow() {
                        break;
                    }
                    rx.changed().await.ok();
                }
            })
            .await?;

        Ok(())
    }
}

#[derive(Clone)]
struct AppState {
    registry: Registry,
    bus: Bus,
    manager: Manager,
    kernel_grpc_addr: String,
}

#[derive(Debug, Deserialize)]
struct HttpInvokeRequest {
    capability: String,
    #[serde(default)]
    payload: String,
    #[serde(default)]
    metadata: std::collections::HashMap<String, String>,
}

#[derive(Debug, Serialize)]
struct HttpInvokeResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<HttpError>,
}

#[derive(Debug, Serialize)]
struct HttpError {
    code: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct HealthzResponse {
    status: String,
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    plugins: Vec<PluginStatus>,
    capabilities: Vec<CapabilityStatus>,
}

#[derive(Debug, Serialize)]
struct PluginStatus {
    name: String,
    state: String,
}

#[derive(Debug, Serialize)]
struct CapabilityStatus {
    name: String,
    version: String,
    plugin: String,
}

#[derive(Debug, Serialize)]
struct RestartResponse {
    status: String,
    plugin: String,
}

async fn healthz() -> Json<HealthzResponse> {
    Json(HealthzResponse {
        status: "ok".into(),
    })
}

async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    let caps = state.registry.list_capabilities();
    let capabilities: Vec<CapabilityStatus> = caps
        .iter()
        .map(|c| CapabilityStatus {
            name: c.name.clone(),
            version: c.version.to_string(),
            plugin: c.plugin_name.clone(),
        })
        .collect();

    let plugin_states = state.manager.list_plugin_states().await;
    let plugins: Vec<PluginStatus> = plugin_states
        .into_iter()
        .map(|(name, state)| PluginStatus {
            name,
            state: format!("{state:?}"),
        })
        .collect();

    Json(StatusResponse {
        plugins,
        capabilities,
    })
}

async fn plugin_restart(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Json<RestartResponse> {
    state.manager.restart_plugin(&name).await;
    Json(RestartResponse {
        status: "restarting".into(),
        plugin: name,
    })
}

async fn invoke(
    State(state): State<AppState>,
    Json(req): Json<HttpInvokeRequest>,
) -> Json<HttpInvokeResponse> {
    let payload = match base64_decode(&req.payload) {
        Some(bytes) => bytes,
        None => {
            return Json(HttpInvokeResponse {
                request_id: None,
                payload: None,
                error: Some(HttpError {
                    code: "INVALID_PAYLOAD".into(),
                    message: "payload must be valid base64".into(),
                }),
            })
        }
    };

    let mut metadata = req.metadata;
    metadata.insert("kernel_grpc_addr".into(), state.kernel_grpc_addr.clone());

    let deadline = metadata
        .get("deadline_unix_ms")
        .and_then(|v| v.parse::<u64>().ok())
        .map(|deadline_ms| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let relative_ms = deadline_ms.saturating_sub(now);
            tokio::time::Instant::now() + Duration::from_millis(relative_ms)
        })
        .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(30));

    let request_id = Uuid::new_v4().to_string();
    tracing::info!(
        "http invoke: capability={} request_id={}",
        req.capability,
        request_id,
    );

    let invocation = Invocation {
        request_id: request_id.clone(),
        capability: req.capability,
        version_constraint: semver::VersionReq::parse("*").unwrap(),
        payload,
        metadata,
        deadline,
    };

    match state.bus.dispatch(invocation).await {
        Ok(payload) => Json(HttpInvokeResponse {
            request_id: Some(request_id),
            payload: Some(base64_encode(&payload)),
            error: None,
        }),
        Err(err) => {
            let (code, message) = match &err {
                InvocationError::NotFound(cap) => ("NOT_FOUND".into(), cap.clone()),
                InvocationError::DeadlineExceeded => {
                    ("DEADLINE_EXCEEDED".into(), "deadline exceeded".into())
                }
                InvocationError::PluginUnhealthy => {
                    ("PLUGIN_UNHEALTHY".into(), "plugin is degraded".into())
                }
                InvocationError::TransportError(msg) => ("TRANSPORT_ERROR".into(), msg.clone()),
                InvocationError::PluginError { code, message } => (code.clone(), message.clone()),
                _ => ("INTERNAL_ERROR".into(), format!("{err}")),
            };

            Json(HttpInvokeResponse {
                request_id: Some(request_id),
                payload: None,
                error: Some(HttpError { code, message }),
            })
        }
    }
}

fn base64_decode(input: &str) -> Option<bytes::Bytes> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .ok()
        .map(bytes::Bytes::from)
}

fn base64_encode(input: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input)
}
