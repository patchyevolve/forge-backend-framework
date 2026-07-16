use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tonic::{transport::Server, Request, Response, Status};

use forgecore_backend_framework_daemon::bus::{Bus, Invocation, InvocationError};
use forgecore_backend_framework_daemon::config::{
    DiscoveredPlugin, PluginCapabilitiesDecl, PluginLifecycleConfig, PluginManifest,
    PluginManifestMeta, PluginTransport,
};
use forgecore_backend_framework_daemon::kernel::{Kernel, KernelConfig};
use forgecore_backend_framework_daemon::lifecycle::{Manager, PluginState};
use forgecore_backend_framework_daemon::registry::Registry;

use forgecore_backend_framework_daemon::proto::forge_plugin_server::{ForgePlugin, ForgePluginServer};
use forgecore_backend_framework_daemon::proto::{
    Capability, DrainRequest, DrainResponse, HealthCheckRequest, HealthCheckResponse,
    InvokeRequest, InvokeResponse, RegisterRequest, RegisterResponse,
};

// ---- helpers ----------------------------------------------------------------

fn memory_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                if l.starts_with("VmRSS:") {
                    l.split_whitespace().nth(1)?.parse().ok()
                } else {
                    None
                }
            })
        })
        .unwrap_or(0)
}

struct Stats {
    count: AtomicU64,
    sum_ns: AtomicU64,
    latencies: Arc<std::sync::Mutex<Vec<u64>>>,
}

impl Stats {
    fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            sum_ns: AtomicU64::new(0),
            latencies: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn record(&self, ns: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_ns.fetch_add(ns, Ordering::Relaxed);
        self.latencies.lock().unwrap().push(ns);
    }

    fn report(&self, label: &str) {
        let count = self.count.load(Ordering::Relaxed);
        let sum_ns = self.sum_ns.load(Ordering::Relaxed);
        let mut lats = self.latencies.lock().unwrap();
        lats.sort_unstable();
        let avg_ns = sum_ns.checked_div(count).unwrap_or(0);
        let p50 = percentile(&lats, 50);
        let p95 = percentile(&lats, 95);
        let p99 = percentile(&lats, 99);
        let min = lats.first().copied().unwrap_or(0);
        let max = lats.last().copied().unwrap_or(0);
        println!(
            "  [{label}] count={count} avg={}µs min={}µs p50={}µs p95={}µs p99={}µs max={}µs",
            avg_ns / 1000,
            min / 1000,
            p50 / 1000,
            p95 / 1000,
            p99 / 1000,
            max / 1000,
        );
    }
}

fn percentile(sorted: &[u64], p: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) as f64 * p as f64 / 100.0).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn inv(cap: &str, deadline_ms: u64, payload: bytes::Bytes) -> Invocation {
    Invocation {
        request_id: "stress".into(),
        capability: cap.into(),
        version_constraint: semver::VersionReq::parse("*").unwrap(),
        payload,
        metadata: std::collections::HashMap::new(),
        deadline: tokio::time::Instant::now() + Duration::from_millis(deadline_ms),
    }
}

fn vec_payload(size: usize) -> bytes::Bytes {
    let mut v = vec![0u8; size];
    for (i, b) in v.iter_mut().enumerate() {
        *b = (i & 0xFF) as u8;
    }
    bytes::Bytes::from(v)
}

async fn join_all<T: Send + 'static>(handles: Vec<tokio::task::JoinHandle<T>>) -> Vec<T> {
    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        results.push(h.await.unwrap());
    }
    results
}

// ---- stress plugins --------------------------------------------------------

struct FailAfterNPlugin {
    cap_name: &'static str,
    invoke_count: AtomicUsize,
    fail_after: usize,
    fail_with: tonic::Code,
}

#[tonic::async_trait]
impl ForgePlugin for FailAfterNPlugin {
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
        let n = self.invoke_count.fetch_add(1, Ordering::Relaxed);
        if n >= self.fail_after {
            return Err(Status::new(self.fail_with, "stressed out"));
        }
        let r = req.into_inner();
        Ok(Response::new(InvokeResponse {
            request_id: r.request_id,
            result: Some(forgecore_backend_framework_daemon::proto::invoke_response::Result::Payload(r.payload)),
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

struct EagerFailPlugin {
    cap_name: &'static str,
}

#[tonic::async_trait]
impl ForgePlugin for EagerFailPlugin {
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
        Err(Status::unavailable("gone"))
    }
    async fn health_check(
        &self,
        _: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        Err(Status::unavailable("gone"))
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
            result: Some(forgecore_backend_framework_daemon::proto::invoke_response::Result::Payload(r.payload)),
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

// ---- server helper ---------------------------------------------------------

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

fn manifest(name: &str, address: &str, lc: PluginLifecycleConfig) -> DiscoveredPlugin {
    DiscoveredPlugin {
        manifest: PluginManifest {
            forge_manifest_version: "1.0".into(),
            plugin: PluginManifestMeta {
                name: name.into(),
                version: "0.1.0".into(),
                description: format!("stress {name}"),
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

fn no_restart_lc() -> PluginLifecycleConfig {
    PluginLifecycleConfig {
        restart_policy: "never".into(),
        restart_backoff_initial_ms: 200,
        restart_backoff_max_ms: 1000,
        restart_max_attempts: 1,
        health_check_interval_ms: 10_000,
        health_check_failure_threshold: 3,
        drain_grace_period_ms: 50,
    }
}

// ---- 1,000 concurrent dispatches ------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_1000_dispatches() {
    let kernel = Kernel::start(KernelConfig::default());
    kernel
        .bus()
        .register_handler(
            "stress.echo",
            |inv: Invocation| async move { Ok(inv.payload) },
        )
        .await;

    let stats = Arc::new(Stats::new());
    let mut handles = Vec::with_capacity(1000);
    let mem_before = memory_kb();

    for _ in 0..1000 {
        let stats = stats.clone();
        let bus = kernel.bus().clone();
        handles.push(tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            let result = bus
                .dispatch(inv("stress.echo", 30_000, vec_payload(128)))
                .await;
            let elapsed = start.elapsed().as_nanos() as u64;
            stats.record(elapsed);
            result.is_ok()
        }));
    }

    let results = join_all(handles).await;
    let successes = results.iter().filter(|ok| **ok).count();
    let failures = results.len() - successes;
    let mem_after = memory_kb();

    stats.report("1,000 concurrent");
    println!(
        "  success={successes}/1000 failure={failures} mem_diff={}KB",
        mem_after.saturating_sub(mem_before)
    );
    assert_eq!(failures, 0, "all 1,000 dispatches should succeed");
}

// ---- multiple concurrent capabilities -------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multiple_concurrent_capabilities() {
    let kernel = Kernel::start(KernelConfig::default());
    for i in 0..10 {
        let cap = format!("cap.{i}");
        let bus = kernel.bus().clone();
        kernel
            .bus()
            .register_handler(&cap, move |inv: Invocation| {
                let bus = bus.clone();
                async move {
                    if inv.capability == "cap.9" {
                        Ok(inv.payload)
                    } else {
                        let n: u32 = inv
                            .capability
                            .rsplit('.')
                            .next()
                            .unwrap_or("0")
                            .parse()
                            .unwrap_or(0);
                        let next = format!("cap.{}", n + 1);
                        bus.dispatch(Invocation {
                            capability: next,
                            ..inv
                        })
                        .await
                    }
                }
            })
            .await;
    }

    let stats = Arc::new(Stats::new());
    let mut handles = Vec::with_capacity(200);

    for _ in 0..200 {
        let stats = stats.clone();
        let bus = kernel.bus().clone();
        handles.push(tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            let result = bus.dispatch(inv("cap.0", 60_000, vec_payload(64))).await;
            let elapsed = start.elapsed().as_nanos() as u64;
            stats.record(elapsed);
            result.is_ok()
        }));
    }

    let results = join_all(handles).await;
    let successes = results.iter().filter(|ok| **ok).count();
    stats.report("200 chained (→ cap.9)");
    println!("  success={successes}/200");
    assert_eq!(successes, 200, "all chained dispatches should succeed");
}

// ---- kill a plugin while requests keep coming -----------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_plugin_under_sustained_load() {
    let plugin = FailAfterNPlugin {
        cap_name: "fragile.test",
        invoke_count: AtomicUsize::new(0),
        fail_after: 50,
        fail_with: tonic::Code::Unavailable,
    };
    let (addr, _sh) = serve(plugin).await;
    let manifest = manifest("fragile", &addr, no_restart_lc());

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());
    manager.start_all(vec![manifest]).await;
    assert_eq!(
        manager.plugin_state("fragile").await,
        Some(PluginState::Ready)
    );

    let mut handles = Vec::with_capacity(200);
    for _ in 0..200 {
        let bus = bus.clone();
        handles.push(tokio::spawn(async move {
            bus.dispatch(inv("fragile.test", 10_000, vec_payload(256)))
                .await
        }));
    }
    let results = join_all(handles).await;
    let successes = results.iter().filter(|r| r.is_ok()).count();
    let failures = results.iter().filter(|r| r.is_err()).count();
    println!("  kill-under-load: ok={successes} err={failures}");
    assert_eq!(successes, 50, "exactly 50 should succeed before failure");
    assert!(failures > 0, "remaining requests should fail");
}

// ---- restart loops while other plugins are busy ----------------------------

fn crash_lc() -> PluginLifecycleConfig {
    PluginLifecycleConfig {
        restart_policy: "on-failure".into(),
        restart_backoff_initial_ms: 100,
        restart_backoff_max_ms: 500,
        restart_max_attempts: 5,
        health_check_interval_ms: 50,
        health_check_failure_threshold: 1,
        drain_grace_period_ms: 50,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rapid_restart_loops_under_load() {
    let (bad_addr, _bad_sh) = serve(EagerFailPlugin {
        cap_name: "loopy.test",
    })
    .await;
    let (good_addr, _good_sh) = serve(HealthyPlugin {
        cap_name: "steady.test",
    })
    .await;

    let lc = crash_lc();
    let loopy = manifest("loopy", &bad_addr, lc);
    let steady = manifest("steady", &good_addr, no_restart_lc());

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());
    manager.start_all(vec![loopy, steady]).await;
    assert_eq!(
        manager.plugin_state("steady").await,
        Some(PluginState::Ready)
    );

    // keep hitting the healthy plugin while the broken one thrashes
    for i in 0..20 {
        let result = bus
            .dispatch(inv("steady.test", 5_000, vec_payload(128)))
            .await;
        assert!(
            result.is_ok(),
            "steady dispatch {i} should succeed during restart loops"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // loopy should've bounced a bunch of times by now
    let loopy_state = manager.plugin_state("loopy").await;
    println!("  restart-loops: steady=Ready loopy={:?}", loopy_state);
    assert!(loopy_state.is_some(), "loopy should have a tracked state");
    assert_eq!(
        manager.plugin_state("steady").await,
        Some(PluginState::Ready)
    );
}

// ---- pushing big payloads through (1MB, 5MB, 10MB) -------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_payloads() {
    let kernel = Kernel::start(KernelConfig::default());
    kernel
        .bus()
        .register_handler(
            "stress.echo",
            |inv: Invocation| async move { Ok(inv.payload) },
        )
        .await;

    for &size in &[1_048_576, 5_242_880, 10_485_760] {
        let mem_before = memory_kb();
        let mut handles = Vec::with_capacity(20);

        for _ in 0..20 {
            let bus = kernel.bus().clone();
            let p = vec_payload(size);
            handles.push(tokio::spawn(async move {
                bus.dispatch(inv("stress.echo", 30_000, p)).await.is_ok()
            }));
        }

        let results = join_all(handles).await;
        let successes = results.iter().filter(|ok| **ok).count();
        let mem_after = memory_kb();
        println!(
            "  payload={}MB: ok={}/20 mem_diff={}KB",
            size / 1_048_576,
            successes,
            mem_after.saturating_sub(mem_before)
        );
        assert_eq!(successes, 20, "all large payload dispatches should succeed");
    }
}

// ---- hammer it with 10k sequential requests, check for leaks ---------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequential_requests_no_leak() {
    let kernel = Kernel::start(KernelConfig::default());
    kernel
        .bus()
        .register_handler(
            "stress.echo",
            |inv: Invocation| async move { Ok(inv.payload) },
        )
        .await;

    let mem_before = memory_kb();
    let count = 10_000u32;

    for i in 0..count {
        let result = kernel
            .bus()
            .dispatch(inv("stress.echo", 30_000, vec_payload(256)))
            .await;
        assert!(result.is_ok(), "request {i} should succeed");
        if i % 2000 == 0 && i > 0 {
            println!("  ... {i}/{count} requests completed");
        }
    }

    let mem_after = memory_kb();
    let mem_delta = mem_after.saturating_sub(mem_before);
    println!("  10,000 sequential: mem_diff={mem_delta}KB");
    assert!(
        mem_delta < 30_720,
        "memory leak detected: +{mem_delta}KB > 30MB"
    );
}

// ---- alternate zero-deadline and long-deadline requests --------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deadline_hammering() {
    let kernel = Kernel::start(KernelConfig::default());
    kernel
        .bus()
        .register_handler(
            "stress.echo",
            |inv: Invocation| async move { Ok(inv.payload) },
        )
        .await;

    let mut successes = 0u32;
    let mut deadline_exceeded = 0u32;

    for i in 0..500 {
        let deadline_ms = if i % 2 == 0 { 0 } else { 30_000 };
        match kernel
            .bus()
            .dispatch(inv("stress.echo", deadline_ms, vec_payload(128)))
            .await
        {
            Ok(_) => successes += 1,
            Err(InvocationError::DeadlineExceeded) => deadline_exceeded += 1,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    println!("  deadline-hammer: ok={successes} deadline_exceeded={deadline_exceeded}/250");
    assert_eq!(
        deadline_exceeded, 250,
        "all expired-deadline requests should fail"
    );
    assert_eq!(successes, 250, "all fresh-deadline requests should succeed");
}

// ---- 5000 concurrent registry lookups --------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registry_contention() {
    let registry = Registry::new();
    for i in 0..100 {
        registry.register(
            format!("cap.{i}"),
            semver::Version::new(1, 0, 0),
            forgecore_backend_framework_daemon::registry::PluginHandle {
                plugin_name: format!("p{i}"),
                instance_id: format!("inst{i}"),
            },
        );
    }

    let stats = Arc::new(Stats::new());
    let counter = Arc::new(AtomicUsize::new(0));
    let mut lookup_handles: Vec<tokio::task::JoinHandle<()>> = Vec::with_capacity(5000);

    for _ in 0..5000 {
        let reg = registry.clone();
        let stats = stats.clone();
        let counter = counter.clone();
        lookup_handles.push(tokio::spawn(async move {
            let idx = counter.fetch_add(1, Ordering::Relaxed) % 100;
            let start = tokio::time::Instant::now();
            let _ = reg.lookup(
                &format!("cap.{idx}"),
                &semver::VersionReq::parse("*").unwrap(),
            );
            let elapsed = start.elapsed().as_nanos() as u64;
            stats.record(elapsed);
        }));
    }

    join_all(lookup_handles).await;
    stats.report("5,000 concurrent registry lookups");
}

// ---- register and deregister from many threads at once ---------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_register_deregister() {
    let registry = Registry::new();
    let mut handles = Vec::with_capacity(2000);

    for i in 0..1000 {
        let reg = registry.clone();
        handles.push(tokio::spawn(async move {
            let handle = forgecore_backend_framework_daemon::registry::PluginHandle {
                plugin_name: format!("p{i}"),
                instance_id: format!("inst{i}"),
            };
            reg.register(
                format!("cap.{i}"),
                semver::Version::new(1, 0, 0),
                handle.clone(),
            );
            reg.deregister(&handle);
        }));
    }
    for _ in 0..1000 {
        let reg = registry.clone();
        handles.push(tokio::spawn(async move {
            let _ = reg.list_capabilities();
            let _ = reg.lookup("cap.0", &semver::VersionReq::parse("*").unwrap());
        }));
    }

    join_all(handles).await;
    let caps = registry.list_capabilities();
    println!(
        "  simultaneous-reg-dereg: {} capabilities remain",
        caps.len()
    );
    assert!(caps.len() <= 1000, "registry should not grow unbounded");
}
