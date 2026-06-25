use std::net::SocketAddr;
use std::time::Duration;

use tonic::{transport::Server, Request, Response, Status};
use uuid::Uuid;

use forge_core::bus::{Bus, Invocation, InvocationError};

use forge_proto::{
    self as proto,
    forge_plugin_server::{ForgePlugin, ForgePluginServer},
    DrainRequest, DrainResponse, HealthCheckRequest, HealthCheckResponse, InvokeRequest,
    InvokeResponse, PluginError, RegisterRequest, RegisterResponse,
};

/// gRPC gateway — accepts InvokeRequests from outside and routes them through the bus.
/// Just translates between gRPC and our internal bus, nothing more.
pub struct GrpcGateway {
    pub bind: String,
    bus: Bus,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
}

impl GrpcGateway {
    pub fn new(bind: String, bus: Bus, shutdown_rx: tokio::sync::watch::Receiver<bool>) -> Self {
        Self {
            bind,
            bus,
            shutdown_rx,
        }
    }

    pub async fn serve(self) -> anyhow::Result<()> {
        let addr: SocketAddr = self.bind.parse()?;
        let kernel_grpc_addr = format!("http://{}:{}", addr.ip(), addr.port());
        let svc = ForgeGatewaySvc {
            bus: self.bus,
            kernel_grpc_addr,
        };

        tracing::info!("gRPC gateway listening on {}", addr);

        Server::builder()
            .add_service(ForgePluginServer::new(svc))
            .serve_with_shutdown(addr, async {
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

struct ForgeGatewaySvc {
    bus: Bus,
    kernel_grpc_addr: String,
}

#[tonic::async_trait]
impl ForgePlugin for ForgeGatewaySvc {
    async fn invoke(
        &self,
        request: Request<InvokeRequest>,
    ) -> Result<Response<InvokeResponse>, Status> {
        let mut req = request.into_inner();

        // Pass along the kernel's gRPC address so the receiving plugin can make
        // outbound calls to other plugins.
        req.metadata
            .insert("kernel_grpc_addr".into(), self.kernel_grpc_addr.clone());

        let deadline = req
            .metadata
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

        let request_id = if req.request_id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            req.request_id.clone()
        };
        tracing::info!(
            "grpc invoke: capability={} request_id={}",
            req.capability,
            request_id,
        );

        let invocation = Invocation {
            request_id: request_id.clone(),
            capability: req.capability,
            version_constraint: semver::VersionReq::parse("*").unwrap(),
            payload: bytes::Bytes::from(req.payload),
            metadata: req.metadata,
            deadline,
        };

        match self.bus.dispatch(invocation).await {
            Ok(payload) => {
                let resp = InvokeResponse {
                    request_id,
                    result: Some(proto::invoke_response::Result::Payload(payload.to_vec())),
                };
                Ok(Response::new(resp))
            }
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
                    InvocationError::PluginError { code, message } => {
                        (code.clone(), message.clone())
                    }
                    _ => ("INTERNAL_ERROR".into(), format!("{err}")),
                };

                let resp = InvokeResponse {
                    request_id,
                    result: Some(proto::invoke_response::Result::Error(PluginError {
                        code,
                        message,
                        details: std::collections::HashMap::new(),
                    })),
                };
                Ok(Response::new(resp))
            }
        }
    }

    async fn register(
        &self,
        _request: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        Err(Status::unimplemented(
            "register is not handled by the external gateway",
        ))
    }

    async fn health_check(
        &self,
        _request: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        Ok(Response::new(HealthCheckResponse {
            healthy: true,
            detail: "gateway ok".into(),
        }))
    }

    async fn drain(
        &self,
        _request: Request<DrainRequest>,
    ) -> Result<Response<DrainResponse>, Status> {
        Err(Status::unimplemented(
            "drain is not handled by the external gateway",
        ))
    }
}
