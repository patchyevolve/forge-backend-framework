//! # Forge Plugin SDK (Rust)
//!
//! Write Forge plugins in Rust. Implement the [`Plugin`] trait, wrap it with
//! [`PluginServer`], and run.
//!
//! ## Quick start
//!
//! ```rust
//! use forge_plugin_sdk_rust::{
//!     Plugin, Capability, InvokeContext, InvokeResult,
//! };
//!
//! struct MyPlugin;
//!
//! #[async_trait::async_trait]
//! impl Plugin for MyPlugin {
//!     fn capabilities(&self) -> Vec<Capability> {
//!         vec![Capability::new("my:action", "1.0.0")]
//!     }
//!
//!     async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
//!         Ok(ctx.payload)  // echo
//!     }
//!
//!     async fn health_check(&self) -> bool { true }
//! }
//! # fn main() {}
//! ```
//!
//! ## Capabilities
//!
//! A capability is something your plugin can do — a named, versioned
//! operation. Register them in [`Plugin::capabilities`] and handle them in
//! [`Plugin::invoke`].
//!
//! ## Calling other plugins
//!
//! Use [`KernelClient`] to invoke capabilities on other plugins through
//! the Forge kernel. The kernel's gRPC address is passed to your plugin
//! in the `metadata` field of [`InvokeContext`].
//!
//! ## Plugin lifecycle
//!
//! 1. Forge spawns your binary as a subprocess
//! 2. Forge calls `register()` to learn your capabilities
//! 3. Forge calls `invoke()` to handle requests
//! 4. Forge calls `health_check()` periodically
//! 5. Forge calls `drain()` before shutting you down

use std::collections::HashMap;

use tonic::transport::{Channel, Endpoint, Server};
use tonic::{Request, Response, Status};

use forge_proto::forge_plugin_client::ForgePluginClient;
use forge_proto::forge_plugin_server::{ForgePlugin, ForgePluginServer};
use forge_proto::{
    self as proto, Capability as ProtoCapability, DrainRequest, DrainResponse, HealthCheckRequest,
    HealthCheckResponse, InvokeRequest, InvokeResponse, RegisterRequest, RegisterResponse,
};

#[doc(no_inline)]
pub use async_trait::async_trait;

/// Something this plugin can do.
#[derive(Debug, Clone)]
pub struct Capability {
    /// Machine-readable name, e.g. `"builtin:credential:list"`.
    pub name: String,
    /// Semver version of this capability.
    pub version: String,
    /// URL or path to a JSON Schema describing valid inputs.
    pub input_schema_ref: String,
    /// URL or path to a JSON Schema describing valid outputs.
    pub output_schema_ref: String,
}

impl Capability {
    /// Create a new capability with the given name and version.
    ///
    /// ```rust
    /// # use forge_plugin_sdk_rust::Capability;
    /// let cap = Capability::new("my:action", "1.0.0");
    /// assert_eq!(cap.name, "my:action");
    /// assert_eq!(cap.version, "1.0.0");
    /// ```
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
    /// Unique identifier for this invocation, used for tracing across plugins.
    pub request_id: String,
    /// Name of the capability being invoked.
    pub capability: String,
    /// Opaque payload bytes (typically JSON or protobuf).
    pub payload: Vec<u8>,
    /// Key-value metadata bag forwarded by the caller.
    pub metadata: HashMap<String, String>,
}

/// Error type that invoke handlers return.
#[derive(Debug, Clone)]
pub struct PluginError {
    /// Machine-readable error code, e.g. `"NOT_FOUND"`, `"TRANSPORT_ERROR"`.
    pub code: String,
    /// Human-readable description of what went wrong.
    pub message: String,
    /// Arbitrary structured details for the caller.
    pub details: HashMap<String, String>,
}

impl PluginError {
    /// Shorthand for a 404-style error.
    ///
    /// ```rust
    /// # use forge_plugin_sdk_rust::PluginError;
    /// let err = PluginError::not_found("capability not registered");
    /// assert_eq!(err.code, "NOT_FOUND");
    /// ```
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
    /// Advertise the capabilities this plugin provides.
    /// Called by the kernel at registration time.
    ///
    /// ```rust
    /// # use forge_plugin_sdk_rust::{Plugin, Capability};
    /// # struct MyPlugin;
    /// # #[async_trait::async_trait]
    /// # impl Plugin for MyPlugin {
    /// fn capabilities(&self) -> Vec<Capability> {
    ///     vec![Capability::new("my:action", "1.0.0")]
    /// }
    /// # async fn invoke(&self, _: forge_plugin_sdk_rust::InvokeContext) -> forge_plugin_sdk_rust::InvokeResult { unimplemented!() }
    /// # async fn health_check(&self) -> bool { true }
    /// # }
    /// ```
    fn capabilities(&self) -> Vec<Capability>;

    /// Handle an invocation for one of the advertised capabilities.
    ///
    /// ```no_run
    /// # use forge_plugin_sdk_rust::{Plugin, Capability, InvokeContext, InvokeResult};
    /// # struct MyPlugin;
    /// # #[async_trait::async_trait]
    /// # impl Plugin for MyPlugin {
    /// async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
    ///     println!("invoked: {}", ctx.capability);
    ///     Ok(ctx.payload)
    /// }
    /// # fn capabilities(&self) -> Vec<Capability> { vec![] }
    /// # async fn health_check(&self) -> bool { true }
    /// # }
    /// ```
    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult;

    /// Return `false` to signal the kernel that this plugin is unhealthy
    /// and should be drained.
    ///
    /// ```no_run
    /// # use forge_plugin_sdk_rust::{Plugin, Capability, InvokeContext, InvokeResult};
    /// # struct MyPlugin;
    /// # #[async_trait::async_trait]
    /// # impl Plugin for MyPlugin {
    /// async fn health_check(&self) -> bool { true }
    /// # fn capabilities(&self) -> Vec<Capability> { vec![] }
    /// # async fn invoke(&self, _: InvokeContext) -> InvokeResult { unimplemented!() }
    /// # }
    /// ```
    async fn health_check(&self) -> bool;

    /// Graceful shutdown hook — called before the kernel forcefully stops
    /// the plugin. The default implementation is a no-op.
    ///
    /// ```no_run
    /// # use forge_plugin_sdk_rust::{Plugin, Capability, InvokeContext, InvokeResult};
    /// # struct MyPlugin;
    /// # #[async_trait::async_trait]
    /// # impl Plugin for MyPlugin {
    /// async fn on_drain(&self) {
    ///     tracing::info!("shutting down gracefully");
    /// }
    /// # fn capabilities(&self) -> Vec<Capability> { vec![] }
    /// # async fn invoke(&self, _: InvokeContext) -> InvokeResult { unimplemented!() }
    /// # async fn health_check(&self) -> bool { true }
    /// # }
    /// ```
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
    /// Wrap a [`Plugin`] implementation ready for serving.
    ///
    /// ```rust
    /// # use forge_plugin_sdk_rust::{Plugin, PluginServer, Capability, InvokeContext, InvokeResult};
    /// # struct MyPlugin;
    /// # #[async_trait::async_trait]
    /// # impl Plugin for MyPlugin {
    /// # fn capabilities(&self) -> Vec<Capability> { vec![] }
    /// # async fn invoke(&self, _: InvokeContext) -> InvokeResult { unimplemented!() }
    /// # async fn health_check(&self) -> bool { true }
    /// # }
    /// let server = PluginServer::new(MyPlugin);
    /// ```
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

        // Strip optional URI scheme prefix so both "http://127.0.0.1:50051"
        // and "127.0.0.1:50051" are accepted
        let addr = addr
            .strip_prefix("http://")
            .or_else(|| addr.strip_prefix("https://"))
            .unwrap_or(&addr);

        if let Some(unix_path) = addr.strip_prefix("unix://") {
            let path = std::path::PathBuf::from(unix_path);
            let _ = std::fs::remove_file(&path);
            let listener = tokio::net::UnixListener::bind(&path)?;
            let incoming = tokio_stream::wrappers::UnixListenerStream::new(listener);
            Server::builder()
                .add_service(ForgePluginServer::new(svc))
                .serve_with_incoming(incoming)
                .await?;
        } else {
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
    /// Establish a gRPC connection to the Forge kernel.
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), anyhow::Error> {
    /// let client = forge_plugin_sdk_rust::KernelClient::connect("http://127.0.0.1:50051").await?;
    /// # Ok(())
    /// # }
    /// ```
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
