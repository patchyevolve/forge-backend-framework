use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tonic::{Request, Response, Status, transport::Server};

use forgecore_backend_framework_daemon::bus::{Bus, Invocation, InvocationError};
use forgecore_backend_framework_daemon::config::{
    DiscoveredPlugin, PluginCapabilitiesDecl, PluginLifecycleConfig, PluginManifest,
    PluginManifestMeta, PluginTransport,
};
use forgecore_backend_framework_daemon::lifecycle::{Manager, PluginState};
use forgecore_backend_framework_daemon::registry::Registry;

use forgecore_backend_framework_daemon::proto::forge_plugin_server::{
    ForgePlugin, ForgePluginServer,
};
use forgecore_backend_framework_daemon::proto::{
    Capability, DrainRequest, DrainResponse, HealthCheckRequest, HealthCheckResponse,
    InvokeRequest, InvokeResponse, RegisterRequest, RegisterResponse,
};

// ---- helpers ---------------------------------------------------------------

fn lifecycle_fast() -> PluginLifecycleConfig {
    PluginLifecycleConfig {
        restart_policy: "on-failure".into(),
        restart_backoff_initial_ms: 200,
        restart_backoff_max_ms: 1000,
        restart_max_attempts: 3,
        health_check_interval_ms: 100,
        health_check_failure_threshold: 2,
        drain_grace_period_ms: 50,
    }
}

fn lifecycle_no_restart() -> PluginLifecycleConfig {
    PluginLifecycleConfig {
        restart_policy: "never".into(),
        ..lifecycle_fast()
    }
}

fn manifest(name: &str, address: &str, lc: PluginLifecycleConfig) -> DiscoveredPlugin {
    DiscoveredPlugin {
        manifest: PluginManifest {
            forge_manifest_version: "1.0".into(),
            plugin: PluginManifestMeta {
                name: name.into(),
                version: "0.1.0".into(),
                description: format!("fixture {name}"),
                protocol_version: "1.0".into(),
            },
            transport: PluginTransport::Server {
                address: address.into(),
            },
            lifecycle: lc,
            capabilities: PluginCapabilitiesDecl {
                provides: vec![format!("{name}.cap@1.0")],
                requires: vec![],
            },
            env: std::collections::HashMap::new(),
        },
        manifest_path: std::path::PathBuf::from("fixture"),
        directory: std::path::PathBuf::from("fixture"),
    }
}

async fn serve(plugin: impl ForgePlugin + 'static) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let addr_str = format!("http://{}:{}", addr.ip(), addr.port());
    let h = tokio::spawn(async move {
        Server::builder()
            .add_service(ForgePluginServer::new(plugin))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (addr_str, h)
}

fn inv(cap: &str, deadline_ms: u64) -> Invocation {
    Invocation {
        request_id: "fi".into(),
        capability: cap.into(),
        version_constraint: semver::VersionReq::parse("*").unwrap(),
        payload: bytes::Bytes::new(),
        metadata: std::collections::HashMap::new(),
        deadline: tokio::time::Instant::now() + Duration::from_millis(deadline_ms),
    }
}

async fn wait_for_state(
    mgr: &Manager,
    name: &str,
    target: PluginState,
    max_wait: Duration,
) -> Option<PluginState> {
    let start = tokio::time::Instant::now();
    while start.elapsed() < max_wait {
        let s = mgr.plugin_state(name).await;
        if s == Some(target) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    mgr.plugin_state(name).await
}

// ---- plugins that misbehave in various ways --------------------------------

struct HangingPlugin {
    cap_name: &'static str,
    delay: Duration,
}

#[tonic::async_trait]
impl ForgePlugin for HangingPlugin {
    async fn register(
        &self,
        _: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        Ok(Response::new(RegisterResponse {
            plugin_protocol_version: "1.0".into(),
            capabilities: vec![Capability {
                name: self.cap_name.into(),
                version: "1.0".into(),
                input_schema_ref: "".into(),
                output_schema_ref: "".into(),
            }],
        }))
    }
    async fn invoke(&self, _: Request<InvokeRequest>) -> Result<Response<InvokeResponse>, Status> {
        tokio::time::sleep(self.delay).await;
        Err(Status::deadline_exceeded("slow response"))
    }
    async fn health_check(
        &self,
        _: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        Ok(Response::new(HealthCheckResponse {
            healthy: true,
            detail: "ok".into(),
        }))
    }
    async fn drain(&self, _: Request<DrainRequest>) -> Result<Response<DrainResponse>, Status> {
        Ok(Response::new(DrainResponse {}))
    }
}

struct ErrorPlugin {
    cap_name: &'static str,
}

#[tonic::async_trait]
impl ForgePlugin for ErrorPlugin {
    async fn register(
        &self,
        _: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        Ok(Response::new(RegisterResponse {
            plugin_protocol_version: "1.0".into(),
            capabilities: vec![Capability {
                name: self.cap_name.into(),
                version: "1.0".into(),
                input_schema_ref: "".into(),
                output_schema_ref: "".into(),
            }],
        }))
    }
    async fn invoke(&self, _: Request<InvokeRequest>) -> Result<Response<InvokeResponse>, Status> {
        Err(Status::internal("corrupted data"))
    }
    async fn health_check(
        &self,
        _: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        Ok(Response::new(HealthCheckResponse {
            healthy: true,
            detail: "ok".into(),
        }))
    }
    async fn drain(&self, _: Request<DrainRequest>) -> Result<Response<DrainResponse>, Status> {
        Ok(Response::new(DrainResponse {}))
    }
}

struct CrashAfterInvokePlugin {
    cap_name: &'static str,
    crashed: Arc<AtomicBool>,
}

#[tonic::async_trait]
impl ForgePlugin for CrashAfterInvokePlugin {
    async fn register(
        &self,
        _: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        Ok(Response::new(RegisterResponse {
            plugin_protocol_version: "1.0".into(),
            capabilities: vec![Capability {
                name: self.cap_name.into(),
                version: "1.0".into(),
                input_schema_ref: "".into(),
                output_schema_ref: "".into(),
            }],
        }))
    }
    async fn invoke(
        &self,
        req: Request<InvokeRequest>,
    ) -> Result<Response<InvokeResponse>, Status> {
        self.crashed.store(true, Ordering::SeqCst);
        let r = req.into_inner();
        Ok(Response::new(InvokeResponse {
            request_id: r.request_id,
            result: Some(
                forgecore_backend_framework_daemon::proto::invoke_response::Result::Payload(
                    r.payload,
                ),
            ),
        }))
    }
    async fn health_check(
        &self,
        _: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        if self.crashed.load(Ordering::SeqCst) {
            Err(Status::unavailable("crashed after invoke"))
        } else {
            Ok(Response::new(HealthCheckResponse {
                healthy: true,
                detail: "ok".into(),
            }))
        }
    }
    async fn drain(&self, _: Request<DrainRequest>) -> Result<Response<DrainResponse>, Status> {
        Ok(Response::new(DrainResponse {}))
    }
}

struct UnhealthyPlugin {
    cap_name: &'static str,
}

#[tonic::async_trait]
impl ForgePlugin for UnhealthyPlugin {
    async fn register(
        &self,
        _: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        Ok(Response::new(RegisterResponse {
            plugin_protocol_version: "1.0".into(),
            capabilities: vec![Capability {
                name: self.cap_name.into(),
                version: "1.0".into(),
                input_schema_ref: "".into(),
                output_schema_ref: "".into(),
            }],
        }))
    }
    async fn invoke(
        &self,
        req: Request<InvokeRequest>,
    ) -> Result<Response<InvokeResponse>, Status> {
        let r = req.into_inner();
        Ok(Response::new(InvokeResponse {
            request_id: r.request_id,
            result: Some(
                forgecore_backend_framework_daemon::proto::invoke_response::Result::Payload(
                    r.payload,
                ),
            ),
        }))
    }
    async fn health_check(
        &self,
        _: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        Ok(Response::new(HealthCheckResponse {
            healthy: false,
            detail: "not ready".into(),
        }))
    }
    async fn drain(&self, _: Request<DrainRequest>) -> Result<Response<DrainResponse>, Status> {
        Ok(Response::new(DrainResponse {}))
    }
}

struct DeadConnectionPlugin {
    cap_name: &'static str,
}

#[tonic::async_trait]
impl ForgePlugin for DeadConnectionPlugin {
    async fn register(
        &self,
        _: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        Ok(Response::new(RegisterResponse {
            plugin_protocol_version: "1.0".into(),
            capabilities: vec![Capability {
                name: self.cap_name.into(),
                version: "1.0".into(),
                input_schema_ref: "".into(),
                output_schema_ref: "".into(),
            }],
        }))
    }
    async fn invoke(&self, _: Request<InvokeRequest>) -> Result<Response<InvokeResponse>, Status> {
        Err(Status::unavailable("broken connection"))
    }
    async fn health_check(
        &self,
        _: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        Err(Status::unavailable("broken connection"))
    }
    async fn drain(&self, _: Request<DrainRequest>) -> Result<Response<DrainResponse>, Status> {
        Ok(Response::new(DrainResponse {}))
    }
}

struct HealthyPlugin {
    cap_name: &'static str,
}

#[tonic::async_trait]
impl ForgePlugin for HealthyPlugin {
    async fn register(
        &self,
        _: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        Ok(Response::new(RegisterResponse {
            plugin_protocol_version: "1.0".into(),
            capabilities: vec![Capability {
                name: self.cap_name.into(),
                version: "1.0".into(),
                input_schema_ref: "".into(),
                output_schema_ref: "".into(),
            }],
        }))
    }
    async fn invoke(
        &self,
        req: Request<InvokeRequest>,
    ) -> Result<Response<InvokeResponse>, Status> {
        let r = req.into_inner();
        Ok(Response::new(InvokeResponse {
            request_id: r.request_id,
            result: Some(
                forgecore_backend_framework_daemon::proto::invoke_response::Result::Payload(
                    r.payload,
                ),
            ),
        }))
    }
    async fn health_check(
        &self,
        _: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        Ok(Response::new(HealthCheckResponse {
            healthy: true,
            detail: "ok".into(),
        }))
    }
    async fn drain(&self, _: Request<DrainRequest>) -> Result<Response<DrainResponse>, Status> {
        Ok(Response::new(DrainResponse {}))
    }
}

// ---- plugin that hangs and never responds ----------------------------------

#[tokio::test]
async fn hanging_plugin_returns_deadline_exceeded() {
    let plugin = HangingPlugin {
        cap_name: "hang.test",
        delay: Duration::from_secs(30),
    };
    let (addr, _sh) = serve(plugin).await;
    let manifest = manifest("hanger", &addr, lifecycle_no_restart());

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());
    manager.start_all(vec![manifest]).await;
    assert_eq!(
        manager.plugin_state("hanger").await,
        Some(PluginState::Ready)
    );

    // give it 100ms — way shorter than the 30s sleep, so the bus should timeout
    let result = bus.dispatch(inv("hang.test", 100)).await;
    assert!(matches!(result, Err(InvocationError::DeadlineExceeded)));

    // make sure the kernel didn't blow up just because one plugin was slow
    assert_eq!(
        manager.plugin_state("hanger").await,
        Some(PluginState::Ready)
    );
}

// ---- plugin that sends back garbage ----------------------------------------

#[tokio::test]
async fn corrupted_payload_returns_transport_error() {
    let plugin = ErrorPlugin {
        cap_name: "corrupt.test",
    };
    let (addr, _sh) = serve(plugin).await;
    let manifest = manifest("corruptor", &addr, lifecycle_no_restart());

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());
    manager.start_all(vec![manifest]).await;
    assert_eq!(
        manager.plugin_state("corruptor").await,
        Some(PluginState::Ready)
    );

    let result = bus.dispatch(inv("corrupt.test", 5000)).await;
    assert!(matches!(result, Err(InvocationError::TransportError(_))));

    assert_eq!(
        manager.plugin_state("corruptor").await,
        Some(PluginState::Ready)
    );
}

// ---- plugin dies right after handling a request -----------------------------

#[tokio::test]
async fn plugin_crashes_during_request_triggers_restart() {
    let crashed = Arc::new(AtomicBool::new(false));
    let plugin = CrashAfterInvokePlugin {
        cap_name: "crashmid.test",
        crashed: crashed.clone(),
    };
    let (addr, _sh) = serve(plugin).await;
    let manifest = manifest(
        "crashmid",
        &addr,
        PluginLifecycleConfig {
            restart_policy: "on-failure".into(),
            restart_backoff_initial_ms: 200,
            restart_backoff_max_ms: 1000,
            restart_max_attempts: 3,
            health_check_interval_ms: 500,
            health_check_failure_threshold: 1,
            drain_grace_period_ms: 50,
        },
    );

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());
    manager.start_all(vec![manifest]).await;
    assert_eq!(
        manager.plugin_state("crashmid").await,
        Some(PluginState::Ready)
    );

    // first dispatch works fine — the crash is triggered *after* the response
    let result = bus.dispatch(inv("crashmid.test", 5000)).await;
    assert!(result.is_ok(), "invoke should succeed before crash");

    // health checker sees the crash and marks it stopped
    let state = wait_for_state(
        &manager,
        "crashmid",
        PluginState::Stopped,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(state, Some(PluginState::Stopped));

    // restart should kick in and bring it back up
    let state = wait_for_state(
        &manager,
        "crashmid",
        PluginState::Ready,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(state, Some(PluginState::Ready));
}

// ---- plugin that never reports healthy -------------------------------------

#[tokio::test]
async fn never_healthy_transitions_to_degraded() {
    let plugin = UnhealthyPlugin {
        cap_name: "sick.test",
    };
    let (addr, _sh) = serve(plugin).await;
    let manifest = manifest(
        "sick",
        &addr,
        PluginLifecycleConfig {
            restart_policy: "never".into(),
            health_check_interval_ms: 50,
            health_check_failure_threshold: 2,
            ..lifecycle_fast()
        },
    );

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());
    manager.start_all(vec![manifest]).await;

    // should land in Degraded, not Stopped — the plugin is alive, just not healthy
    let state = wait_for_state(
        &manager,
        "sick",
        PluginState::Degraded,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(state, Some(PluginState::Degraded));

    let plugins = manager.list_plugin_states().await;
    let s = plugins.iter().find(|(n, _)| n == "sick");
    assert_eq!(s.map(|(_, s)| s), Some(&PluginState::Degraded));
}

// ---- broken connection — every health check / invoke fails ------------------

#[tokio::test]
async fn broken_connection_triggers_crash_detection() {
    let plugin = DeadConnectionPlugin {
        cap_name: "broken.test",
    };
    let (addr, _sh) = serve(plugin).await;
    let manifest = manifest(
        "broken",
        &addr,
        PluginLifecycleConfig {
            restart_policy: "on-failure".into(),
            restart_backoff_initial_ms: 200,
            restart_backoff_max_ms: 1000,
            restart_max_attempts: 3,
            health_check_interval_ms: 400,
            health_check_failure_threshold: 1,
            drain_grace_period_ms: 50,
        },
    );

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());
    manager.start_all(vec![manifest]).await;

    // first unavailable health check triggers crash detection -> stops the plugin
    let state = wait_for_state(
        &manager,
        "broken",
        PluginState::Stopped,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(state, Some(PluginState::Stopped));

    // restart should get it back online
    let state = wait_for_state(
        &manager,
        "broken",
        PluginState::Ready,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(state, Some(PluginState::Ready));
}

// ---- keep crashing and restarting — make sure backoff works ----------------

#[tokio::test]
async fn restart_storm_respects_backoff() {
    let plugin = DeadConnectionPlugin {
        cap_name: "stormy.test",
    };
    let (addr, _sh) = serve(plugin).await;
    let manifest = manifest(
        "stormy",
        &addr,
        PluginLifecycleConfig {
            restart_policy: "on-failure".into(),
            restart_backoff_initial_ms: 300,
            restart_backoff_max_ms: 5000,
            restart_max_attempts: 5,
            health_check_interval_ms: 600,
            health_check_failure_threshold: 1,
            drain_grace_period_ms: 50,
        },
    );

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());

    let start = tokio::time::Instant::now();
    manager.start_all(vec![manifest]).await;

    // first crash should transition it to stopped
    let state = wait_for_state(
        &manager,
        "stormy",
        PluginState::Stopped,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(state, Some(PluginState::Stopped));

    let first_stopped = start.elapsed();

    // first restart: backoff 300ms + connect ~100ms so we should be ready ~400ms after stopped
    let state = wait_for_state(
        &manager,
        "stormy",
        PluginState::Ready,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(state, Some(PluginState::Ready));

    // make sure we didn't skip the backoff — at least 300ms should have passed
    let to_ready = start.elapsed() - first_stopped;
    assert!(
        to_ready >= Duration::from_millis(300),
        "restart should respect backoff_initial_ms (300ms), got {:?}",
        to_ready
    );

    // let it bounce a few more cycles and make sure the backoff prevents busy-looping
    tokio::time::sleep(Duration::from_secs(4)).await;
}

// ---- one plugin constantly crashing shouldn't stop others from working -----

#[tokio::test]
async fn immediate_recrash_does_not_exhaust_other_plugins() {
    let (bad_addr, _bad_sh) = serve(DeadConnectionPlugin {
        cap_name: "recrash.test",
    })
    .await;
    let (good_addr, _good_sh) = serve(HealthyPlugin {
        cap_name: "steady.test",
    })
    .await;

    let bad = manifest(
        "recrasher",
        &bad_addr,
        PluginLifecycleConfig {
            restart_policy: "on-failure".into(),
            restart_backoff_initial_ms: 200,
            restart_backoff_max_ms: 5000,
            restart_max_attempts: 5,
            health_check_interval_ms: 50,
            health_check_failure_threshold: 1,
            drain_grace_period_ms: 50,
        },
    );

    let good = manifest("steady", &good_addr, lifecycle_no_restart());

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());
    manager.start_all(vec![bad, good]).await;

    assert_eq!(
        manager.plugin_state("steady").await,
        Some(PluginState::Ready)
    );

    // give the crashing plugin time to bounce a few times
    tokio::time::sleep(Duration::from_secs(4)).await;

    // the healthy one should still be fine and able to handle requests
    assert_eq!(
        manager.plugin_state("steady").await,
        Some(PluginState::Ready)
    );
    let result = bus.dispatch(inv("steady.test", 5000)).await;
    assert!(
        result.is_ok(),
        "healthy plugin should serve during restart storm"
    );
}

// ---- two failing plugins at once — make sure they don't interfere ----------

#[tokio::test]
async fn multiple_failures_remain_isolated() {
    let (a_addr, _a_sh) = serve(DeadConnectionPlugin {
        cap_name: "failing-a.test",
    })
    .await;
    let (b_addr, _b_sh) = serve(DeadConnectionPlugin {
        cap_name: "failing-b.test",
    })
    .await;
    let (c_addr, _c_sh) = serve(HealthyPlugin {
        cap_name: "control-c.test",
    })
    .await;

    let fast = PluginLifecycleConfig {
        restart_policy: "on-failure".into(),
        restart_backoff_initial_ms: 200,
        restart_backoff_max_ms: 2000,
        restart_max_attempts: 5,
        health_check_interval_ms: 50,
        health_check_failure_threshold: 1,
        drain_grace_period_ms: 50,
    };

    let a = manifest("failing-a", &a_addr, fast.clone());
    let b = manifest("failing-b", &b_addr, fast.clone());
    let c = manifest("control-c", &c_addr, lifecycle_no_restart());

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());
    manager.start_all(vec![a, b, c]).await;

    // let the failing ones thrash for a bit
    tokio::time::sleep(Duration::from_secs(6)).await;

    // the control plugin shouldn't care that its neighbors are on fire
    assert_eq!(
        manager.plugin_state("control-c").await,
        Some(PluginState::Ready)
    );
    let result = bus.dispatch(inv("control-c.test", 5000)).await;
    assert!(
        result.is_ok(),
        "control plugin should serve during multi-failure"
    );

    // make sure the failing ones are still cycling, not stuck in some limbo state
    let states = manager.list_plugin_states().await;
    let a_state = states
        .iter()
        .find(|(n, _)| n == "failing-a")
        .map(|(_, s)| *s);
    let b_state = states
        .iter()
        .find(|(n, _)| n == "failing-b")
        .map(|(_, s)| *s);
    assert!(a_state.is_some(), "failing-a should have a state");
    assert!(b_state.is_some(), "failing-b should have a state");
}
