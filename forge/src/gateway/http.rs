use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::IntoResponse,
    routing::{get, post},
};
use prometheus::{
    Registry as PromRegistry, register_counter_vec_with_registry,
    register_histogram_vec_with_registry,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::bus::{Bus, Invocation, InvocationError};
use crate::config::RouteDef;
use crate::lifecycle::Manager;
use crate::registry::Registry;

/// Parse a "name@version" capability string into (name, VersionReq).
/// If no "@" is present, uses name as-is with "*" (any version).
fn parse_route_capability(s: &str) -> (String, semver::VersionReq) {
    match s.split_once('@') {
        Some((name, ver)) => (
            name.to_string(),
            semver::VersionReq::parse(ver).unwrap_or(semver::VersionReq::STAR),
        ),
        None => (s.to_string(), semver::VersionReq::STAR),
    }
}

/// HTTP (REST) gateway — exposes health, status, invoke, and plugin-management
/// endpoints plus declarative HTTP routing.
pub struct HttpGateway {
    bind: String,
    _tls: bool,
    _tls_cert_path: Option<String>,
    _tls_key_path: Option<String>,
    registry: Registry,
    bus: Bus,
    manager: Manager,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    kernel_grpc_addr: String,
    static_dir: Option<String>,
    cors_allowed_origins: Vec<String>,
    rate_limit_per_minute: u64,
    max_body_size: u64,
    routes: Vec<RouteDef>,
}

impl HttpGateway {
    /// Build the HTTP gateway.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bind: String,
        tls: bool,
        tls_cert_path: Option<String>,
        tls_key_path: Option<String>,
        registry: Registry,
        bus: Bus,
        manager: Manager,
        shutdown_rx: tokio::sync::watch::Receiver<bool>,
        kernel_grpc_addr: String,
        static_dir: Option<String>,
        cors_allowed_origins: Vec<String>,
        rate_limit_per_minute: u64,
        max_body_size: u64,
        routes: Vec<RouteDef>,
    ) -> Self {
        Self {
            bind,
            _tls: tls,
            _tls_cert_path: tls_cert_path,
            _tls_key_path: tls_key_path,
            registry,
            bus,
            manager,
            shutdown_rx,
            kernel_grpc_addr,
            static_dir,
            cors_allowed_origins,
            rate_limit_per_minute,
            max_body_size,
            routes,
        }
    }

    /// Start the HTTP server. Blocks until the shutdown signal is received.
    pub async fn serve(self) -> anyhow::Result<()> {
        let addr: SocketAddr = self.bind.parse()?;

        let rate_limiter = if self.rate_limit_per_minute > 0 {
            Some(Arc::new(RateLimiter::new(self.rate_limit_per_minute)))
        } else {
            None
        };

        let compiled_routes: Vec<CompiledRoute> =
            self.routes.iter().map(CompiledRoute::compile).collect();

        if !compiled_routes.is_empty() {
            tracing::info!("Declarative routes: {} registered", compiled_routes.len());
        }

        let metrics_registry = PromRegistry::new();
        let app_state = AppState {
            registry: self.registry,
            bus: self.bus,
            manager: self.manager,
            kernel_grpc_addr: self.kernel_grpc_addr,
            rate_limiter,
            routes: Arc::new(compiled_routes),
            max_body_size: self.max_body_size,
            metrics_registry,
        };

        // Build router with all routes + fallback, THEN inject state.
        // state type (`AppState`) is correctly inferred for the fallback handler.
        // Calling `.fallback()` on a `Router<()>` would fail because the fallback
        // handler expects `State<AppState>` which cannot be extracted from `()`.
        // Similarly, `.layer()` must be called BEFORE `.with_state()`, and the
        // final router must NOT be assigned to a previously-typed variable,
        // otherwise the compiler infers S2 = AppState instead of S2 = ().
        let need_cors = !self.cors_allowed_origins.is_empty();
        let cors_origins = Arc::new(self.cors_allowed_origins.clone());

        // Register default prometheus metrics
        let _http_requests = register_counter_vec_with_registry!(
            "forge_http_requests_total",
            "Total HTTP requests",
            &["method", "path", "status"],
            app_state.metrics_registry.clone(),
        )
        .unwrap();
        let _http_duration = register_histogram_vec_with_registry!(
            "forge_http_request_duration_seconds",
            "HTTP request duration in seconds",
            &["method", "path"],
            app_state.metrics_registry.clone(),
        )
        .unwrap();

        let mut rb = Router::new()
            .route("/healthz", get(healthz))
            .route("/v1/healthz", get(healthz))
            .route("/v1/status", get(status))
            .route("/v1/invoke", post(invoke))
            .route("/v1/plugins/{name}/restart", post(plugin_restart))
            .route("/metrics", get(metrics_handler))
            .fallback(declarative_handler);

        // Body size middleware (checks Content-Length before reading body).
        // Uses `from_fn_with_state` so the closure can return a Response directly.
        if self.max_body_size > 0 {
            let max = self.max_body_size;
            rb = rb.layer(axum::middleware::from_fn(
                move |req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| {
                    let max = max;
                    async move {
                        if let Some(cl) = req.headers().get(axum::http::header::CONTENT_LENGTH)
                            && let Some(len) = cl.to_str().ok().and_then(|s| s.parse::<u64>().ok())
                            && len > max
                        {
                            let mut r = axum::http::Response::new(axum::body::Body::empty());
                            *r.status_mut() = axum::http::StatusCode::PAYLOAD_TOO_LARGE;
                            return r;
                        }
                        next.run(req).await
                    }
                },
            ));
            tracing::info!("Max body size: {} bytes", self.max_body_size);
        }

        if need_cors {
            let origins = cors_origins.clone();
            rb = rb.layer(axum::middleware::from_fn(
                move |req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| {
                    let origins = origins.clone();
                    async move {
                        let origin_val = req
                            .headers()
                            .get(axum::http::header::ORIGIN)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        let is_preflight =
                            req.method() == axum::http::Method::OPTIONS && !origin_val.is_empty();
                        let is_wildcard = origins.iter().any(|o| o == "*");
                        let allowed = is_wildcard || origins.iter().any(|o| o == &origin_val);
                        if is_preflight {
                            let mut resp = axum::http::Response::new(axum::body::Body::empty());
                            if allowed {
                                let origin_hdr = if is_wildcard {
                                    "*".to_string()
                                } else {
                                    origin_val.clone()
                                };
                                resp.headers_mut().insert(
                                    axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
                                    origin_hdr.parse().unwrap(),
                                );
                                resp.headers_mut().insert(
                                    axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
                                    "GET, POST, PUT, DELETE, PATCH, OPTIONS".parse().unwrap(),
                                );
                                resp.headers_mut().insert(
                                    axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,
                                    "Content-Type, Authorization, X-Request-ID".parse().unwrap(),
                                );
                            }
                            return resp;
                        }
                        let mut resp = next.run(req).await;
                        if allowed {
                            let origin_hdr = if is_wildcard {
                                "*".to_string()
                            } else {
                                origin_val.clone()
                            };
                            resp.headers_mut().insert(
                                axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
                                origin_hdr.parse().unwrap(),
                            );
                        }
                        resp
                    }
                },
            ));
            tracing::info!("CORS enabled for origins: {:?}", cors_origins);
        }

        // Use a fresh binding so S2 is NOT constrained by the LHS type.
        let router = rb.with_state(app_state);

        // Wrap with static file serving if a directory is configured.
        // Try ServeDir first; if the file doesn't exist, fall through to the API router.
        let router = if let Some(dir) = self.static_dir {
            let svc = tower_http::services::ServeDir::new(&dir)
                .not_found_service(router.into_service());
            Router::new().fallback_service(svc)
        } else {
            router
        };

        let tls_enabled = self._tls;

        tracing::info!(
            "HTTP gateway listening on {} (TLS: {})",
            addr,
            if tls_enabled { "enabled" } else { "disabled" }
        );

        if self.rate_limit_per_minute > 0 {
            tracing::info!(
                "Rate limit enabled: {} requests/min per IP",
                self.rate_limit_per_minute
            );
        }

        let listener = tokio::net::TcpListener::bind(addr).await?;

        // Use into_make_service_with_connect_info so handlers can extract
        // ConnectInfo<SocketAddr> for per-IP rate limiting.
        let svc = router
            .clone()
            .into_make_service_with_connect_info::<SocketAddr>();

        if tls_enabled {
            serve_tls(
                listener,
                router,
                self.shutdown_rx,
                self._tls_cert_path.as_deref().unwrap_or("server.crt"),
                self._tls_key_path.as_deref().unwrap_or("server.key"),
            )
            .await?;
        } else {
            axum::serve(listener, svc)
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
        }

        Ok(())
    }
}

// TLS serve helper

async fn serve_tls(
    listener: tokio::net::TcpListener,
    router: axum::Router<()>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    cert_path: &str,
    key_path: &str,
) -> anyhow::Result<()> {
    let mut cert_file = std::io::BufReader::new(std::fs::File::open(cert_path)?);
    let certs = rustls_pemfile::certs(&mut cert_file).collect::<Result<Vec<_>, _>>()?;
    let mut key_file = std::io::BufReader::new(std::fs::File::open(key_path)?);
    let key = rustls_pemfile::pkcs8_private_keys(&mut key_file)
        .next()
        .ok_or_else(|| anyhow::anyhow!("no private key found in {}", key_path))??;

    use rustls::pki_types::PrivateKeyDer;
    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, PrivateKeyDer::Pkcs8(key))?;

    let tls_acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config));
    // Limit concurrent TLS handshakes and connections
    let semaphore = Arc::new(tokio::sync::Semaphore::new(256));

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    return Ok(());
                }
            }
            result = listener.accept() => {
                let (tcp, remote_addr) = result?;
                let semaphore = semaphore.clone();
                let tls_acceptor = tls_acceptor.clone();
                let router = router.clone();
                tokio::spawn(async move {
                    let _permit = match semaphore.try_acquire_owned() {
                        Ok(p) => p,
                        Err(_) => {
                            tracing::warn!("too many TLS connections, dropping {remote_addr}");
                            return;
                        }
                    };
                    let tls_stream = match tls_acceptor.accept(tcp).await {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!("TLS accept error: {e}");
                            return;
                        }
                    };
                    let io = hyper_util::rt::TokioIo::new(tls_stream);
                    let svc = hyper::service::service_fn(move |req| {
                        use tonic::codegen::Service;
                        let mut router = router.clone();
                        let addr = remote_addr;
                        async move {
                            let mut req = req;
                            req.extensions_mut()
                                .insert(axum::extract::connect_info::ConnectInfo::<SocketAddr>(addr));
                            let resp = router.call(req).await;
                            Ok::<_, std::convert::Infallible>(resp.unwrap())
                        }
                    });
                    if let Err(e) = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await
                    {
                        tracing::warn!("TLS connection error: {e}");
                    }
                });
            }
        }
    }
}

// Route pattern matching

/// A compiled route pattern for efficient matching.
#[derive(Debug, Clone)]
struct CompiledRoute {
    route: RouteDef,
    /// Segments of the path pattern.
    /// Static segments are `Some(name)`, param segments are `None`.
    segments: Vec<PathSegment>,
}

#[derive(Debug, Clone)]
enum PathSegment {
    Static(String),
    Param(String),
}

impl CompiledRoute {
    fn compile(route: &RouteDef) -> Self {
        let segments = compile_path(&route.path);
        Self {
            route: route.clone(),
            segments,
        }
    }
}

/// Parse a path pattern like `/api/items/{id}` into segments.
fn compile_path(path: &str) -> Vec<PathSegment> {
    path.trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|segment| {
            if (segment.starts_with('{') && segment.ends_with('}')) || segment.starts_with(':') {
                let name = segment
                    .trim_start_matches('{')
                    .trim_start_matches(':')
                    .trim_end_matches('}');
                PathSegment::Param(name.to_string())
            } else {
                PathSegment::Static(segment.to_string())
            }
        })
        .collect()
}

/// Match a request path against compiled route segments.
/// Returns extracted path parameters on success.
fn match_path(segments: &[PathSegment], path: &str) -> Option<HashMap<String, String>> {
    let request_segments: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    if request_segments.len() != segments.len() {
        return None;
    }

    let mut params = HashMap::new();
    for (i, segment) in segments.iter().enumerate() {
        match segment {
            PathSegment::Static(expected) => {
                if request_segments[i] != expected.as_str() {
                    return None;
                }
            }
            PathSegment::Param(name) => {
                params.insert(name.clone(), request_segments[i].to_string());
            }
        }
    }

    Some(params)
}

// Shared application state

#[derive(Clone)]
struct AppState {
    registry: Registry,
    bus: Bus,
    manager: Manager,
    kernel_grpc_addr: String,
    rate_limiter: Option<Arc<RateLimiter>>,
    routes: Arc<Vec<CompiledRoute>>,
    max_body_size: u64,
    metrics_registry: PromRegistry,
}

// Global rate limiter (counts across all IPs)
struct RateLimiter {
    max_per_minute: u64,
    entries: Arc<tokio::sync::Mutex<HashMap<IpAddr, RateLimitEntry>>>,
}

#[derive(Debug, Clone)]
struct RateLimitEntry {
    count: u64,
    window_start: tokio::time::Instant,
}

impl RateLimiter {
    fn new(max_per_minute: u64) -> Self {
        let entries = Arc::new(tokio::sync::Mutex::new(
            HashMap::<IpAddr, RateLimitEntry>::new(),
        ));
        if max_per_minute > 0 {
            let entries = entries.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    let now = tokio::time::Instant::now();
                    let mut map = entries.lock().await;
                    map.retain(|_, e| now < e.window_start + Duration::from_secs(60));
                }
            });
        }
        Self {
            max_per_minute,
            entries,
        }
    }

    async fn check(&self, ip: IpAddr) -> Result<(), ()> {
        if self.max_per_minute == 0 {
            return Ok(());
        }
        let mut entries = self.entries.lock().await;
        let now = tokio::time::Instant::now();
        let entry = entries.entry(ip).or_insert(RateLimitEntry {
            count: 0,
            window_start: now,
        });
        if now >= entry.window_start + Duration::from_secs(60) {
            entry.count = 0;
            entry.window_start = now;
        }
        entry.count += 1;
        if entry.count > self.max_per_minute {
            return Err(());
        }
        Ok(())
    }
}

// Request / response types

#[derive(Debug, Deserialize)]
struct HttpInvokeRequest {
    capability: String,
    #[serde(default)]
    payload: String,
    #[serde(default)]
    metadata: HashMap<String, String>,
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

// Error → HTTP status mapping

fn invocation_error_status(err: &InvocationError) -> StatusCode {
    match err {
        InvocationError::NotFound(_) => StatusCode::NOT_FOUND,
        InvocationError::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
        InvocationError::PluginUnhealthy => StatusCode::SERVICE_UNAVAILABLE,
        InvocationError::TransportError(_) => StatusCode::BAD_GATEWAY,
        InvocationError::PluginError { .. } => StatusCode::BAD_REQUEST,
    }
}

fn invocation_error_pair(err: &InvocationError) -> (String, String) {
    match &err {
        InvocationError::NotFound(cap) => ("NOT_FOUND".into(), cap.clone()),
        InvocationError::DeadlineExceeded => {
            ("DEADLINE_EXCEEDED".into(), "deadline exceeded".into())
        }
        InvocationError::PluginUnhealthy => {
            ("PLUGIN_UNHEALTHY".into(), "plugin is degraded".into())
        }
        InvocationError::TransportError(msg) => ("TRANSPORT_ERROR".into(), msg.clone()),
        InvocationError::PluginError { code, message } => (code.clone(), message.clone()),
    }
}

// Built-in endpoint handlers

async fn metrics_handler(State(state): State<AppState>) -> axum::response::Response {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();
    let mut buf = Vec::new();
    if let Err(e) = encoder.encode(&state.metrics_registry.gather(), &mut buf) {
        return axum::http::Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(axum::body::Body::from(format!("metrics error: {e}")))
            .unwrap();
    }
    axum::http::Response::builder()
        .header("Content-Type", "text/plain; charset=utf-8")
        .body(axum::body::Body::from(buf))
        .unwrap()
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
        .map(|(name, pstate)| PluginStatus {
            name,
            state: format!("{pstate:?}"),
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

#[derive(Debug, Serialize)]
struct RestartResponse {
    status: String,
    plugin: String,
}

async fn invoke(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<HttpInvokeRequest>,
) -> impl IntoResponse {
    if let Some(limiter) = &state.rate_limiter
        && limiter.check(addr.ip()).await.is_err()
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(HttpInvokeResponse {
                request_id: None,
                payload: None,
                error: Some(HttpError {
                    code: "RATE_LIMITED".into(),
                    message: "too many requests — try again later".into(),
                }),
            }),
        );
    }

    let payload = match base64_decode(&req.payload) {
        Some(bytes) => bytes,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(HttpInvokeResponse {
                    request_id: None,
                    payload: None,
                    error: Some(HttpError {
                        code: "INVALID_PAYLOAD".into(),
                        message: "payload must be valid base64".into(),
                    }),
                }),
            );
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
        Ok(payload) => (
            StatusCode::OK,
            Json(HttpInvokeResponse {
                request_id: Some(request_id),
                payload: Some(base64_encode(&payload)),
                error: None,
            }),
        ),
        Err(err) => {
            let status = invocation_error_status(&err);
            let (code, message) = invocation_error_pair(&err);
            (
                status,
                Json(HttpInvokeResponse {
                    request_id: Some(request_id),
                    payload: None,
                    error: Some(HttpError { code, message }),
                }),
            )
        }
    }
}

// Declarative route handler (fallback for all undeclared paths)

/// Fallback handler that matches incoming requests against declarative routes
/// defined in `forge.toml`. Extracts path params, query params, and JSON body,
/// optionally calls an auth capability, then dispatches to the target capability.
async fn declarative_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let path = uri.path();

    let matched = state.routes.iter().find(|cr| {
        let method_match = cr.route.method.eq_ignore_ascii_case(method.as_str());
        method_match && match_path(&cr.segments, path).is_some()
    });

    let route = match matched {
        Some(cr) => cr,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "request_id": null,
                    "error": { "code": "NOT_FOUND", "message": "no matching route" },
                })),
            );
        }
    };

    let path_params = match_path(&route.segments, path).unwrap();

    let query_params: HashMap<String, String> = uri.query().map(parse_query).unwrap_or_default();

    if state.max_body_size > 0 && body.len() > state.max_body_size as usize {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "request_id": null,
                "error": {
                    "code": "PAYLOAD_TOO_LARGE",
                    "message": format!("request body exceeds max size of {} bytes", state.max_body_size),
                },
            })),
        );
    }

    let body_value: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };

    let request_id = Uuid::new_v4().to_string();

    if let Some(auth_cap) = &route.route.auth {
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        let auth_payload = serde_json::to_vec(&serde_json::json!({"token": token})).unwrap();

        let auth_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let (auth_name, auth_constraint) = parse_route_capability(auth_cap);
        let auth_invocation = Invocation {
            request_id: request_id.clone(),
            capability: auth_name,
            version_constraint: auth_constraint,
            payload: bytes::Bytes::from(auth_payload),
            metadata: HashMap::from([("kernel_grpc_addr".into(), state.kernel_grpc_addr.clone())]),
            deadline: auth_deadline,
        };

        match state.bus.dispatch(auth_invocation).await {
            Ok(payload) => {
                let auth_result: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                let valid = auth_result
                    .get("valid")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !valid {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({
                            "error": "UNAUTHORIZED",
                            "message": "invalid token",
                            "request_id": request_id,
                        })),
                    );
                }
                tracing::debug!("auth passed for {request_id}");
            }
            Err(err) => {
                let (code, message) = invocation_error_pair(&err);
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": code,
                        "message": message,
                        "request_id": request_id,
                    })),
                );
            }
        }
    }

    let merged_payload = if body_value.is_object() {
        let mut obj = body_value.as_object().unwrap().clone();
        for (k, v) in &path_params {
            obj.insert(k.clone(), Value::String(v.clone()));
        }
        Value::Object(obj)
    } else if body_value.is_null() {
        let mut obj = serde_json::Map::new();
        for (k, v) in &path_params {
            obj.insert(k.clone(), Value::String(v.clone()));
        }
        if !query_params.is_empty() {
            let q: Value = serde_json::to_value(&query_params).unwrap();
            if let Some(q_obj) = q.as_object() {
                for (k, v) in q_obj {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
        Value::Object(obj)
    } else {
        body_value
    };

    let metadata: HashMap<String, String> = query_params
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .chain(std::iter::once((
            "kernel_grpc_addr".into(),
            state.kernel_grpc_addr.clone(),
        )))
        .collect();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let payload_bytes = serde_json::to_vec(&merged_payload).unwrap();

    tracing::info!(
        "route invoke: {} {} -> capability={} request_id={}",
        route.route.method,
        route.route.path,
        route.route.capability,
        request_id,
    );

    let (cap_name, cap_constraint) = parse_route_capability(&route.route.capability);
    let invocation = Invocation {
        request_id: request_id.clone(),
        capability: cap_name,
        version_constraint: cap_constraint,
        payload: bytes::Bytes::from(payload_bytes),
        metadata,
        deadline,
    };

    match state.bus.dispatch(invocation).await {
        Ok(payload) => {
            let response_payload: Value = serde_json::from_slice(&payload)
                .unwrap_or(Value::String(String::from_utf8_lossy(&payload).to_string()));
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "request_id": request_id,
                    "payload": response_payload,
                })),
            )
        }
        Err(err) => {
            let status = invocation_error_status(&err);
            let (code, message) = invocation_error_pair(&err);
            (
                status,
                Json(serde_json::json!({
                    "request_id": request_id,
                    "error": { "code": code, "message": message },
                })),
            )
        }
    }
}

// Utility functions

/// Parse a URL query string into a key-value map.
fn parse_query(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if let Some(eq) = pair.find('=') {
            let key = url_decode(&pair[..eq]);
            let value = url_decode(&pair[eq + 1..]);
            map.insert(key, value);
        } else if !pair.is_empty() {
            map.insert(url_decode(pair), String::new());
        }
    }
    map
}

/// Minimal percent-decoding for query string values.
fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        match b {
            b'+' => result.push(' '),
            b'%' => {
                let hi = chars.next().and_then(hex_val).unwrap_or(0);
                let lo = chars.next().and_then(hex_val).unwrap_or(0);
                result.push((hi << 4 | lo) as char);
            }
            _ => result.push(b as char),
        }
    }
    result
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
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

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        let original = b"hello world";
        let encoded = base64_encode(original);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(&decoded[..], original);
    }

    #[test]
    fn base64_decode_invalid() {
        assert!(base64_decode("not-valid-base64!!!").is_none());
    }

    #[test]
    fn error_status_mapping() {
        assert_eq!(
            invocation_error_status(&InvocationError::NotFound("test".into())),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            invocation_error_status(&InvocationError::DeadlineExceeded),
            StatusCode::GATEWAY_TIMEOUT
        );
        assert_eq!(
            invocation_error_status(&InvocationError::PluginUnhealthy),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn error_pair_extraction() {
        let (code, msg) = invocation_error_pair(&InvocationError::NotFound("foo".into()));
        assert_eq!(code, "NOT_FOUND");
        assert_eq!(msg, "foo");
    }

    #[tokio::test]
    async fn healthz_response() {
        let Json(resp) = healthz().await;
        assert_eq!(resp.status, "ok");
    }

    #[test]
    fn compile_simple_path() {
        let segments = compile_path("/api/items");
        assert_eq!(segments.len(), 2);
        assert!(matches!(&segments[0], PathSegment::Static(s) if s == "api"));
        assert!(matches!(&segments[1], PathSegment::Static(s) if s == "items"));
    }

    #[test]
    fn compile_path_with_params() {
        let segments = compile_path("/api/items/{id}");
        assert_eq!(segments.len(), 3);
        assert!(matches!(&segments[2], PathSegment::Param(n) if n == "id"));
    }

    #[test]
    fn compile_path_with_colon_params() {
        let segments = compile_path("/api/items/:id");
        assert_eq!(segments.len(), 3);
        assert!(matches!(&segments[2], PathSegment::Param(n) if n == "id"));
    }

    #[test]
    fn compile_path_handles_empty() {
        let segments = compile_path("");
        assert!(segments.is_empty());
    }

    #[test]
    fn match_static_path() {
        let segments = compile_path("/api/items");
        let params = match_path(&segments, "/api/items");
        assert!(params.is_some());
        assert!(params.unwrap().is_empty());
    }

    #[test]
    fn match_path_with_params() {
        let segments = compile_path("/api/items/{id}");
        let params = match_path(&segments, "/api/items/42");
        assert!(params.is_some());
        let params = params.unwrap();
        assert_eq!(params.get("id").unwrap(), "42");
    }

    #[test]
    fn match_path_mismatch_length() {
        let segments = compile_path("/api/items");
        let params = match_path(&segments, "/api/items/42");
        assert!(params.is_none());
    }

    #[test]
    fn match_path_mismatch_segment() {
        let segments = compile_path("/api/items");
        let params = match_path(&segments, "/api/users");
        assert!(params.is_none());
    }

    #[test]
    fn match_path_multiple_params() {
        let segments = compile_path("/api/{resource}/{id}");
        let params = match_path(&segments, "/api/users/99");
        assert!(params.is_some());
        let params = params.unwrap();
        assert_eq!(params.get("resource").unwrap(), "users");
        assert_eq!(params.get("id").unwrap(), "99");
    }

    #[tokio::test]
    async fn rate_limiter_allows_within_limit() {
        let limiter = RateLimiter::new(100);
        let ip: IpAddr = "192.168.1.1".parse().unwrap();
        for _ in 0..100 {
            assert!(limiter.check(ip).await.is_ok());
        }
    }

    #[tokio::test]
    async fn rate_limiter_blocks_excess() {
        let limiter = RateLimiter::new(3);
        let ip: IpAddr = "192.168.1.1".parse().unwrap();
        assert!(limiter.check(ip).await.is_ok());
        assert!(limiter.check(ip).await.is_ok());
        assert!(limiter.check(ip).await.is_ok());
        assert!(limiter.check(ip).await.is_err());
    }

    #[tokio::test]
    async fn rate_limiter_per_ip() {
        let limiter = RateLimiter::new(2);
        let ip1: IpAddr = "192.168.1.1".parse().unwrap();
        let ip2: IpAddr = "192.168.1.2".parse().unwrap();

        assert!(limiter.check(ip1).await.is_ok());
        assert!(limiter.check(ip1).await.is_ok());
        assert!(limiter.check(ip1).await.is_err());

        assert!(limiter.check(ip2).await.is_ok());
        assert!(limiter.check(ip2).await.is_ok());
        assert!(limiter.check(ip2).await.is_err());
    }
}
