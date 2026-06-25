use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "tonic")]
use tokio::sync::Mutex;
use tokio::time::Instant;

#[cfg(feature = "tonic")]
use tokio::time::timeout;

#[cfg(feature = "tonic")]
use tonic::transport::Channel;

#[cfg(feature = "tonic")]
use forge_proto::forge_plugin_client::ForgePluginClient;
#[cfg(feature = "tonic")]
use forge_proto::{invoke_response, InvokeRequest};

#[cfg(feature = "tonic")]
use crate::registry::PluginHandle;
use crate::registry::Registry;

/// In-process handler for a capability — no gRPC involved.
pub type HandlerFn = Arc<
    dyn Fn(
            Invocation,
        ) -> Pin<Box<dyn Future<Output = Result<bytes::Bytes, InvocationError>> + Send>>
        + Send
        + Sync,
>;

/// An invocation moving through the bus.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub request_id: String,
    pub capability: String,
    pub version_constraint: semver::VersionReq,
    pub payload: bytes::Bytes,
    pub metadata: std::collections::HashMap<String, String>,
    pub deadline: Instant,
}

impl Invocation {
    /// Quick-build an invocation with default deadline and no metadata — handy for tests or embedding.
    pub fn simple(capability: &str, payload: impl Into<bytes::Bytes>) -> Self {
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            capability: capability.to_string(),
            version_constraint: semver::VersionReq::parse("*").unwrap(),
            payload: payload.into(),
            metadata: std::collections::HashMap::new(),
            deadline: Instant::now() + Duration::from_secs(30),
        }
    }
}

/// Typed errors that can come back from an invocation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InvocationError {
    #[error("capability not found: {0}")]
    NotFound(String),

    #[error("deadline exceeded")]
    DeadlineExceeded,

    #[error("plugin is unhealthy (degraded)")]
    PluginUnhealthy,

    #[error("transport error: {0}")]
    TransportError(String),

    #[error("plugin error: code={code}, message={message}")]
    PluginError { code: String, message: String },
}

/// The bus's view of a connected plugin. Holds a tonic Channel so the bus can make gRPC clients per invocation without borrowing headaches.
#[cfg(feature = "tonic")]
#[derive(Debug, Clone)]
pub struct PluginConnection {
    pub handle: PluginHandle,
    pub channel: Channel,
}

/// The internal async message bus — routes invocations between plugins.
#[derive(Clone)]
pub struct Bus {
    #[cfg_attr(not(feature = "tonic"), allow(dead_code))]
    registry: Registry,
    #[cfg(feature = "tonic")]
    connections: Arc<Mutex<HashMap<String, PluginConnection>>>,
    handlers: Arc<tokio::sync::RwLock<HashMap<String, HandlerFn>>>,
    #[allow(dead_code)]
    default_timeout: Duration,
}

impl std::fmt::Debug for Bus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bus").finish_non_exhaustive()
    }
}

impl Bus {
    #[must_use]
    pub fn new(registry: Registry) -> Self {
        Self {
            registry,
            #[cfg(feature = "tonic")]
            connections: Arc::new(Mutex::new(HashMap::new())),
            handlers: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            default_timeout: Duration::from_secs(30),
        }
    }

    /// Register a local (in-process) handler for a capability. When dispatch() is called for this capability, the handler runs directly instead of going through gRPC.
    pub async fn register_handler<F, Fut>(&self, name: &str, f: F)
    where
        F: Fn(Invocation) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<bytes::Bytes, InvocationError>> + Send + 'static,
    {
        let handler: HandlerFn = Arc::new(move |inv| Box::pin(f(inv)));
        self.handlers
            .write()
            .await
            .insert(name.to_string(), handler);
    }

    /// Register a plugin connection so the bus can route invocations to it.
    #[cfg(feature = "tonic")]
    pub async fn register_connection(&self, conn: PluginConnection) {
        let key = connection_key(&conn.handle);
        self.connections.lock().await.insert(key, conn);
    }

    /// Remove a plugin connection — called when a plugin hits STOPPED or DEGRADED.
    #[cfg(feature = "tonic")]
    pub async fn remove_connection(&self, handle: &PluginHandle) {
        let key = connection_key(handle);
        self.connections.lock().await.remove(&key);
    }

    /// Send an invocation through the bus. Local in-process handlers are checked first, then the gRPC path.
    pub async fn dispatch(&self, invocation: Invocation) -> Result<bytes::Bytes, InvocationError> {
        // Always check the deadline first — applies to both paths
        if Instant::now() >= invocation.deadline {
            return Err(InvocationError::DeadlineExceeded);
        }

        // Local handlers get priority
        {
            let handlers = self.handlers.read().await;
            if let Some(handler) = handlers.get(&invocation.capability) {
                return handler(invocation).await;
            }
        }

        // Without tonic, there's nothing left to try after local handlers
        #[cfg(not(feature = "tonic"))]
        {
            return Err(InvocationError::NotFound(invocation.capability.clone()));
        }

        // gRPC path — only works with tonic feature enabled
        #[cfg(feature = "tonic")]
        {
            // Resolve the capability name to whoever provides it
            let plugin_handle = self
                .registry
                .lookup(&invocation.capability, &invocation.version_constraint)
                .ok_or_else(|| InvocationError::NotFound(invocation.capability.clone()))?;

            let now = Instant::now();
            if now >= invocation.deadline {
                return Err(InvocationError::DeadlineExceeded);
            }

            // Grab the channel for this plugin and call Invoke
            let conn_key = connection_key(&plugin_handle);
            let channel = {
                let conns = self.connections.lock().await;
                conns
                    .get(&conn_key)
                    .map(|c| c.channel.clone())
                    .ok_or_else(|| InvocationError::TransportError("plugin not connected".into()))?
            };

            let mut client = ForgePluginClient::new(channel);

            tracing::debug!(
                "bus dispatch: capability={} request_id={}",
                invocation.capability,
                invocation.request_id,
            );

            let grpc_req = tonic::Request::new(InvokeRequest {
                request_id: invocation.request_id.clone(),
                capability: invocation.capability.clone(),
                payload: invocation.payload.to_vec(),
                metadata: invocation.metadata,
            });

            // Clamp the timeout to whatever's left before the deadline
            let remaining = invocation.deadline.saturating_duration_since(now);
            let timeout_dur = std::cmp::min(remaining, self.default_timeout);

            // Fire the gRPC call with a timeout
            let response = match timeout(timeout_dur, client.invoke(grpc_req)).await {
                Ok(Ok(resp)) => resp.into_inner(),
                Ok(Err(status)) => {
                    return Err(InvocationError::TransportError(format!(
                        "gRPC invoke failed: {status}"
                    )));
                }
                Err(_elapsed) => {
                    return Err(InvocationError::DeadlineExceeded);
                }
            };

            // Unpack the response — either payload bytes or a typed error
            match response.result {
                Some(invoke_response::Result::Payload(payload)) => Ok(bytes::Bytes::from(payload)),
                Some(invoke_response::Result::Error(err)) => Err(InvocationError::PluginError {
                    code: err.code,
                    message: err.message,
                }),
                None => Err(InvocationError::TransportError(
                    "empty invoke response".into(),
                )),
            }
        }
    }
}

#[cfg(feature = "tonic")]
fn connection_key(handle: &PluginHandle) -> String {
    format!("{}:{}", handle.plugin_name, handle.instance_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_to_nonexistent_plugin() {
        let registry = Registry::new();
        let bus = Bus::new(registry);

        let inv = Invocation {
            request_id: "test-req".into(),
            capability: "does.not.exist".into(),
            version_constraint: semver::VersionReq::parse("*").unwrap(),
            payload: bytes::Bytes::new(),
            metadata: HashMap::new(),
            deadline: Instant::now() + Duration::from_secs(5),
        };

        let result = bus.dispatch(inv).await;
        assert!(matches!(result, Err(InvocationError::NotFound(_))));
    }

    #[tokio::test]
    async fn deadline_already_past() {
        let registry = Registry::new();
        let bus = Bus::new(registry.clone());

        // Can't spin up a real gRPC connection in a unit test, so we just verify
        // the deadline check fires first. The capability exists but nothing's
        // connected — it'd hit TransportError if the deadline check passed.
        let handle = PluginHandle {
            plugin_name: "test".into(),
            instance_id: "inst-1".into(),
        };
        registry.register("test.cap".into(), semver::Version::new(1, 0, 0), handle);

        let inv = Invocation {
            request_id: "test-req".into(),
            capability: "test.cap".into(),
            version_constraint: semver::VersionReq::parse("*").unwrap(),
            payload: bytes::Bytes::new(),
            metadata: HashMap::new(),
            deadline: Instant::now() - Duration::from_secs(1),
        };

        let result = bus.dispatch(inv).await;
        assert!(matches!(result, Err(InvocationError::DeadlineExceeded)));
    }
}
