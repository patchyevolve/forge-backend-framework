use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tonic::{transport::Server, Request, Response, Status};

use forge_backend::bus::{Bus, Invocation};
use forge_backend::config::{
    DiscoveredPlugin, PluginCapabilitiesDecl, PluginLifecycleConfig, PluginManifest,
    PluginManifestMeta, PluginTransport,
};
use forge_backend::lifecycle::{Manager, PluginState};
use forge_backend::registry::Registry;

use forge_proto::forge_plugin_server::{ForgePlugin, ForgePluginServer};
use forge_proto::{
    Capability, DrainRequest, DrainResponse, HealthCheckRequest, HealthCheckResponse,
    InvokeRequest, InvokeResponse, RegisterRequest, RegisterResponse,
};

fn build_manifest(name: &str, address: &str) -> DiscoveredPlugin {
    DiscoveredPlugin {
        manifest: PluginManifest {
            forge_manifest_version: "1.0".into(),
            plugin: PluginManifestMeta {
                name: name.into(),
                version: "0.1.0".into(),
                description: format!("Test plugin {name}"),
                protocol_version: "1.0".into(),
            },
            transport: PluginTransport::Server {
                address: address.into(),
            },
            lifecycle: PluginLifecycleConfig {
                restart_policy: "never".into(),
                restart_backoff_initial_ms: 100,
                restart_backoff_max_ms: 1000,
                restart_max_attempts: 1,
                health_check_interval_ms: 500,
                health_check_failure_threshold: 3,
                drain_grace_period_ms: 100,
            },
            capabilities: PluginCapabilitiesDecl {
                provides: vec!["forge.example.echo@1.0".into()],
                requires: vec![],
            },
            env: std::collections::HashMap::new(),
        },
        manifest_path: PathBuf::from("test-fixture"),
        directory: PathBuf::from("test-fixture"),
    }
}

async fn start_fake_server(
    plugin: impl ForgePlugin + 'static,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        Server::builder()
            .add_service(ForgePluginServer::new(plugin))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (addr, handle)
}

#[tokio::test]
async fn lifecycle_connects_to_plugin_and_routes_invocation() {
    let capability = Capability {
        name: "forge.example.echo".into(),
        version: "1.0.0".into(),
        input_schema_ref: "raw text".into(),
        output_schema_ref: "raw text".into(),
    };

    let plugin = FakePlugin {
        capabilities: vec![capability.clone()],
        drain_called: Arc::new(AtomicBool::new(false)),
    };

    let (addr, _server_handle) = start_fake_server(plugin).await;
    let addr_str = format!("http://{}:{}", addr.ip(), addr.port());

    let manifest = build_manifest("test-echo", &addr_str);

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());

    manager.start_all(vec![manifest]).await;

    let state = manager.plugin_state("test-echo").await;
    assert_eq!(state, Some(PluginState::Ready));

    let inv = Invocation {
        request_id: "test-req-001".into(),
        capability: "forge.example.echo".into(),
        version_constraint: semver::VersionReq::parse("^1.0").unwrap(),
        payload: bytes::Bytes::from("hello"),
        metadata: std::collections::HashMap::new(),
        deadline: tokio::time::Instant::now() + Duration::from_secs(5),
    };

    let result = bus.dispatch(inv).await;
    let response = result.expect("dispatch should succeed over real gRPC connection");
    assert_eq!(
        String::from_utf8_lossy(&response),
        "HELLO",
        "response should be uppercased echo from the fake server"
    );

    let plugins = manager.list_plugin_states().await;
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].1, PluginState::Ready);
}

#[tokio::test]
async fn shutdown_calls_drain_on_plugin() {
    let drain_called = Arc::new(AtomicBool::new(false));
    let drain_called_clone = drain_called.clone();

    let plugin = DrainTrackingPlugin {
        drain_called: drain_called_clone,
    };

    let (addr, _server_handle) = start_fake_server(plugin).await;
    let addr_str = format!("http://{}:{}", addr.ip(), addr.port());

    let manifest = build_manifest("drain-test", &addr_str);

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());

    manager.start_all(vec![manifest]).await;
    assert_eq!(
        manager.plugin_state("drain-test").await,
        Some(PluginState::Ready)
    );

    // shut it down and make sure the drain rpc actually gets called, with a timeout so we don't hang
    tokio::time::timeout(Duration::from_secs(5), async {
        manager.shutdown_all().await;
    })
    .await
    .expect("shutdown_all should complete within 5 seconds");

    assert!(
        drain_called.load(Ordering::SeqCst),
        "Drain RPC must have been called on the plugin during shutdown"
    );

    assert_eq!(
        manager.plugin_state("drain-test").await,
        Some(PluginState::Stopped)
    );
}
#[tokio::test]
async fn restart_state_machine() {
    let capability = Capability {
        name: "forge.example.echo".into(),
        version: "1.0.0".into(),
        input_schema_ref: "raw text".into(),
        output_schema_ref: "raw text".into(),
    };

    let plugin = FakePlugin {
        capabilities: vec![capability.clone()],
        drain_called: Arc::new(AtomicBool::new(false)),
    };

    let (addr, _server_handle) = start_fake_server(plugin).await;
    let addr_str = format!("http://{}:{}", addr.ip(), addr.port());

    let manifest = build_manifest("restart-test", &addr_str);

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());

    // phase 1 — just start it, should land in Ready
    manager.start_all(vec![manifest.clone()]).await;
    assert_eq!(
        manager.plugin_state("restart-test").await,
        Some(PluginState::Ready),
        "Phase 1: plugin should reach Ready after start_all"
    );

    // phase 2 — manually restart: drain, deregister, land in Discovered
    manager.restart_plugin("restart-test").await;

    // after restart_plugin we should be back in discovered, waiting to reconnect
    assert_eq!(
        manager.plugin_state("restart-test").await,
        Some(PluginState::Discovered),
        "Phase 2: after restart_plugin, state should be Discovered"
    );

    // phase 3 — fire up start_all again and watch it go through the full reconnect cycle
    manager.start_all(vec![manifest]).await;
    assert_eq!(
        manager.plugin_state("restart-test").await,
        Some(PluginState::Ready),
        "Phase 3: after second start_all, plugin should be Ready"
    );

    // phase 4 — make sure dispatching still works on the fresh connection
    let inv = Invocation {
        request_id: "test-req-002".into(),
        capability: "forge.example.echo".into(),
        version_constraint: semver::VersionReq::parse("^1.0").unwrap(),
        payload: bytes::Bytes::from("hello"),
        metadata: std::collections::HashMap::new(),
        deadline: tokio::time::Instant::now() + Duration::from_secs(5),
    };
    let result = bus.dispatch(inv).await;
    let response = result.expect("dispatch should succeed after restart");
    assert_eq!(
        String::from_utf8_lossy(&response),
        "HELLO",
        "Phase 4: uppercased echo from restarted plugin"
    );
}

// ---- test fakes ------------------------------------------------------------

struct FakePlugin {
    capabilities: Vec<Capability>,
    drain_called: Arc<AtomicBool>,
}

#[tonic::async_trait]
impl ForgePlugin for FakePlugin {
    async fn register(
        &self,
        _request: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        Ok(Response::new(RegisterResponse {
            plugin_protocol_version: "1.0".into(),
            capabilities: self.capabilities.clone(),
        }))
    }

    async fn invoke(
        &self,
        request: Request<InvokeRequest>,
    ) -> Result<Response<InvokeResponse>, Status> {
        let req = request.into_inner();
        let text = String::from_utf8_lossy(&req.payload);
        let echoed = text.to_uppercase();
        Ok(Response::new(InvokeResponse {
            request_id: req.request_id,
            result: Some(forge_proto::invoke_response::Result::Payload(
                echoed.into_bytes(),
            )),
        }))
    }

    async fn health_check(
        &self,
        _request: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        Ok(Response::new(HealthCheckResponse {
            healthy: true,
            detail: "test server ok".into(),
        }))
    }

    async fn drain(
        &self,
        _request: Request<DrainRequest>,
    ) -> Result<Response<DrainResponse>, Status> {
        self.drain_called.store(true, Ordering::SeqCst);
        Ok(Response::new(DrainResponse {}))
    }
}

struct DrainTrackingPlugin {
    drain_called: Arc<AtomicBool>,
}

#[tonic::async_trait]
impl ForgePlugin for DrainTrackingPlugin {
    async fn register(
        &self,
        _request: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        Ok(Response::new(RegisterResponse {
            plugin_protocol_version: "1.0".into(),
            capabilities: vec![],
        }))
    }

    async fn invoke(
        &self,
        _request: Request<InvokeRequest>,
    ) -> Result<Response<InvokeResponse>, Status> {
        Err(Status::unimplemented("not needed for drain test"))
    }

    async fn health_check(
        &self,
        _request: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        Ok(Response::new(HealthCheckResponse {
            healthy: true,
            detail: "ok".into(),
        }))
    }

    async fn drain(
        &self,
        _request: Request<DrainRequest>,
    ) -> Result<Response<DrainResponse>, Status> {
        self.drain_called.store(true, Ordering::SeqCst);
        Ok(Response::new(DrainResponse {}))
    }
}
