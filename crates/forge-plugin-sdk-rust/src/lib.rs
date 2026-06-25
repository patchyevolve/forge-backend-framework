use std::collections::HashMap;

use tonic::transport::{Channel, Endpoint, Server};
use tonic::{Request, Response, Status};

use forge_proto::forge_plugin_client::ForgePluginClient;
use forge_proto::forge_plugin_server::{ForgePlugin, ForgePluginServer};
use forge_proto::{
    self as proto, Capability as ProtoCapability, DrainRequest, DrainResponse, HealthCheckRequest,
    HealthCheckResponse, InvokeRequest, InvokeResponse, RegisterRequest, RegisterResponse,
};

pub use async_trait::async_trait;

/// Something this plugin can do.
#[derive(Debug, Clone)]
pub struct Capability {
    pub name: String,
    pub version: String,
    pub input_schema_ref: String,
    pub output_schema_ref: String,
}

impl Capability {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            input_schema_ref: String::new(),
            output_schema_ref: String::new(),
        }
    }
}

impl From<Capability> for ProtoCapability {
    fn from(c: Capability) -> Self {
        ProtoCapability {
            name: c.name,
            version: c.version,
            input_schema_ref: c.input_schema_ref,
            output_schema_ref: c.output_schema_ref,
        }
    }
}

/// What gets passed to a plugin's invoke handler.
#[derive(Debug, Clone)]
pub struct InvokeContext {
    pub request_id: String,
    pub capability: String,
    pub payload: Vec<u8>,
    pub metadata: HashMap<String, String>,
}

/// Error type that invoke handlers return.
#[derive(Debug, Clone)]
pub struct PluginError {
    pub code: String,
    pub message: String,
    pub details: HashMap<String, String>,
}

impl PluginError {
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "NOT_FOUND".into(),
            message: message.into(),
            details: HashMap::new(),
        }
    }
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plugin error [{}]: {}", self.code, self.message)
    }
}

impl std::error::Error for PluginError {}

impl From<PluginError> for proto::PluginError {
    fn from(e: PluginError) -> Self {
        proto::PluginError {
            code: e.code,
            message: e.message,
            details: e.details,
        }
    }
}

/// Shorthand for what invoke returns.
pub type InvokeResult = Result<Vec<u8>, PluginError>;

/// The main trait — implement this to make a plugin.
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    fn capabilities(&self) -> Vec<Capability>;

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult;

    async fn health_check(&self) -> bool;

    async fn on_drain(&self) {}
}

struct PluginServiceWrapper<P: Plugin> {
    plugin: P,
}

#[tonic::async_trait]
impl<P: Plugin> ForgePlugin for PluginServiceWrapper<P> {
    async fn register(
        &self,
        _request: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        let capabilities: Vec<ProtoCapability> = self
            .plugin
            .capabilities()
            .into_iter()
            .map(Into::into)
            .collect();
        Ok(Response::new(RegisterResponse {
            plugin_protocol_version: "1.0".into(),
            capabilities,
        }))
    }

    async fn invoke(
        &self,
        request: Request<InvokeRequest>,
    ) -> Result<Response<InvokeResponse>, Status> {
        let req = request.into_inner();
        let ctx = InvokeContext {
            request_id: req.request_id.clone(),
            capability: req.capability,
            payload: req.payload,
            metadata: req.metadata,
        };
        match self.plugin.invoke(ctx).await {
            Ok(payload) => Ok(Response::new(InvokeResponse {
                request_id: req.request_id,
                result: Some(proto::invoke_response::Result::Payload(payload)),
            })),
            Err(err) => Ok(Response::new(InvokeResponse {
                request_id: req.request_id,
                result: Some(proto::invoke_response::Result::Error(err.into())),
            })),
        }
    }

    async fn health_check(
        &self,
        _request: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        let healthy = self.plugin.health_check().await;
        Ok(Response::new(HealthCheckResponse {
            healthy,
            detail: "ok".into(),
        }))
    }

    async fn drain(
        &self,
        _request: Request<DrainRequest>,
    ) -> Result<Response<DrainResponse>, Status> {
        self.plugin.on_drain().await;
        Ok(Response::new(DrainResponse {}))
    }
}

/// Wraps a Plugin and runs it as a gRPC server.
pub struct PluginServer<P: Plugin> {
    plugin: P,
}

impl<P: Plugin> PluginServer<P> {
    #[must_use]
    pub fn new(plugin: P) -> Self {
        Self { plugin }
    }

    /// Start the server — Shape A (plugin acts as the server).
    /// Picks up the address from `FORGE_LISTEN_ADDR`. Supports TCP
    /// (`127.0.0.1:50051`) or Unix sockets (`unix:///path/to/sock`).
    /// Falls back to `unix:///tmp/forge-plugin.sock` if nothing is set.
    pub async fn serve_shape_a(self) -> anyhow::Result<()> {
        let addr = std::env::var("FORGE_LISTEN_ADDR")
            .unwrap_or_else(|_| "unix:///tmp/forge-plugin.sock".into());

        tracing::info!("forge-plugin-sdk: listening on {}", addr);

        let svc = PluginServiceWrapper {
            plugin: self.plugin,
        };

        if let Some(unix_path) = addr.strip_prefix("unix://") {
            // Unix socket path
            let path = std::path::PathBuf::from(unix_path);
            // Clean up any leftover socket file from last time
            let _ = std::fs::remove_file(&path);
            let listener = tokio::net::UnixListener::bind(&path)?;
            let incoming = tokio_stream::wrappers::UnixListenerStream::new(listener);
            Server::builder()
                .add_service(ForgePluginServer::new(svc))
                .serve_with_incoming(incoming)
                .await?;
        } else {
            // Plain TCP socket
            Server::builder()
                .add_service(ForgePluginServer::new(svc))
                .serve(addr.parse()?)
                .await?;
        }

        Ok(())
    }
}

/// Lets plugins call other plugins through the kernel.
/// Point it at the kernel's gRPC address (from InvokeContext metadata)
/// and use `invoke` to dispatch calls to any registered plugin.
pub struct KernelClient {
    channel: Channel,
}

impl KernelClient {
    pub async fn connect(grpc_addr: &str) -> Result<Self, anyhow::Error> {
        let channel = Endpoint::new(grpc_addr.to_string())?.connect().await?;
        Ok(Self { channel })
    }

    /// Call a capability via the kernel's gRPC gateway.
    /// Pass through the original request_id from InvokeContext so traces span
    /// the whole plugin chain.
    pub async fn invoke(
        &self,
        capability: &str,
        payload: Vec<u8>,
        metadata: HashMap<String, String>,
        request_id: &str,
    ) -> InvokeResult {
        let mut client = ForgePluginClient::new(self.channel.clone());
        let req = tonic::Request::new(InvokeRequest {
            request_id: request_id.to_string(),
            capability: capability.into(),
            payload,
            metadata,
        });
        match client.invoke(req).await {
            Ok(resp) => {
                let inner = resp.into_inner();
                match inner.result {
                    Some(proto::invoke_response::Result::Payload(p)) => Ok(p),
                    Some(proto::invoke_response::Result::Error(e)) => Err(PluginError {
                        code: e.code,
                        message: e.message,
                        details: e.details,
                    }),
                    None => Err(PluginError {
                        code: "EMPTY_RESPONSE".into(),
                        message: "plugin returned empty response".into(),
                        details: HashMap::new(),
                    }),
                }
            }
            Err(status) => Err(PluginError {
                code: "TRANSPORT_ERROR".into(),
                message: status.to_string(),
                details: HashMap::new(),
            }),
        }
    }
}
