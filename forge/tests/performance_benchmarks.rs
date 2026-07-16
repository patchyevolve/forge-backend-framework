//! Latency & throughput benchmarks for forge-backend.
//!
//! Run:  cargo test --test performance_benchmarks -- --nocapture
//!
//! Spits out min / p50 / p95 / p99 / max / mean latency + throughput for each scenario.
//! At the end it prints the environment (CPU, OS, rustc version, build profile).

use std::sync::atomic::Ordering;
use std::time::Duration;

#[cfg(feature = "gateway")]
use tonic::{transport::Server, Request, Response, Status};

use forge::bus::{Bus, Invocation};
use forge::config::{
    DiscoveredPlugin, PluginCapabilitiesDecl, PluginLifecycleConfig, PluginManifest,
    PluginManifestMeta, PluginTransport,
};
use forge::kernel::{Kernel, KernelConfig};
#[cfg(feature = "gateway")]
use forge::lifecycle::{Manager, PluginState};
use forge::registry::Registry;

#[cfg(feature = "gateway")]
use forge::proto::forge_plugin_server::{ForgePlugin, ForgePluginServer};
#[cfg(feature = "gateway")]
use forge::proto::{
    Capability, DrainRequest, DrainResponse, HealthCheckRequest, HealthCheckResponse,
    InvokeRequest, InvokeResponse, RegisterRequest, RegisterResponse,
};

// ---- helpers ---------------------------------------------------------------

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

fn environment_info() -> String {
    let mut info = String::new();
    info.push_str("═══ Benchmark environment ═══\n");

    // rustc --version output
    info.push_str(&format!("Rust: {}\n", rustc_version()));

    // debug vs release, features enabled
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let tonic = if cfg!(feature = "gateway") {
        "tonic"
    } else {
        "no-tonic"
    };
    info.push_str(&format!("Profile: {profile}, features: [{tonic}]\n"));

    // /proc/cpuinfo — model name + core count
    if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
        let cores = cpuinfo
            .lines()
            .filter(|l| l.starts_with("processor"))
            .count();
        if let Some(model) = cpuinfo.lines().find(|l| l.starts_with("model name")) {
            let val = model.split(':').nth(1).unwrap_or("?").trim();
            info.push_str(&format!("CPU: {val} ({cores} cores)\n"));
        }
    }

    // kernel version from /proc/version
    if let Ok(v) = std::fs::read_to_string("/proc/version") {
        info.push_str(&format!("OS: {}\n", v.trim()));
    }

    // unix timestamp in the output so we know when the bench was run
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    info.push_str(&format!("Date (epoch): {now}\n"));
    info
}

fn rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "unknown".into())
        .trim()
        .to_string()
}

fn inv(cap: &str, deadline_ms: u64, payload: bytes::Bytes) -> Invocation {
    Invocation {
        request_id: "bench".into(),
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

// ---- benchmark runner ------------------------------------------------------

struct BenchResult {
    label: String,
    count: usize,
    sum_ns: u64,
    latencies: Vec<u64>,
    warmup: usize,
}

impl BenchResult {
    fn new(label: &str, warmup: usize, capacity: usize) -> Self {
        Self {
            label: label.into(),
            count: 0,
            sum_ns: 0,
            latencies: Vec::with_capacity(capacity),
            warmup,
        }
    }

    fn record(&mut self, ns: u64) {
        self.count += 1;
        self.sum_ns += ns;
        self.latencies.push(ns);
    }

    fn report(&self) {
        if self.count == 0 {
            println!("  [{}] — no samples —", self.label);
            return;
        }
        let mut sorted = self.latencies.clone();
        sorted.sort_unstable();
        let avg_ns = self.sum_ns / self.count as u64;
        let p50 = percentile(&sorted, 50);
        let p95 = percentile(&sorted, 95);
        let p99 = percentile(&sorted, 99);
        let min = sorted.first().copied().unwrap_or(0);
        let max = sorted.last().copied().unwrap_or(0);
        let total_sec = self.sum_ns as f64 / 1_000_000_000.0;
        let throughput = if total_sec > 0.0 {
            self.count as f64 / total_sec
        } else {
            0.0
        };

        println!("═══ {} ═══", self.label);
        println!("  samples: {}  (warmup: {})", self.count, self.warmup);
        println!("  min:   {:>8.1} µs", min as f64 / 1000.0);
        println!("  p50:   {:>8.1} µs", p50 as f64 / 1000.0);
        println!("  p95:   {:>8.1} µs", p95 as f64 / 1000.0);
        println!("  p99:   {:>8.1} µs", p99 as f64 / 1000.0);
        println!("  max:   {:>8.1} µs", max as f64 / 1000.0);
        println!("  mean:  {:>8.1} µs", avg_ns as f64 / 1000.0);
        println!("  thrpt: {:>8.0} req/s", throughput);
    }
}

fn percentile(sorted: &[u64], p: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) as f64 * p as f64 / 100.0).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

async fn bench_fn<F, Fut>(label: &str, warmup: usize, samples: usize, f: F) -> BenchResult
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // run warmup iterations before measuring — let the jit/caches settle
    for _ in 0..warmup {
        f().await;
    }

    let mut result = BenchResult::new(label, warmup, samples);
    for _ in 0..samples {
        let start = tokio::time::Instant::now();
        f().await;
        let elapsed = start.elapsed().as_nanos() as u64;
        result.record(elapsed);
    }
    result.report();
    result
}

#[cfg(feature = "gateway")]
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

#[cfg(feature = "gateway")]
fn manifest(name: &str, address: &str, lc: PluginLifecycleConfig) -> DiscoveredPlugin {
    DiscoveredPlugin {
        manifest: PluginManifest {
            forge_manifest_version: "1.0".into(),
            plugin: PluginManifestMeta {
                name: name.into(),
                version: "0.1.0".into(),
                description: format!("bench {name}"),
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

// ---- how fast does kernel::start() return? ---------------------------------

#[tokio::test]
async fn kernel_startup_time() {
    let warmup = 10;
    let samples = 100;
    bench_fn("Kernel startup", warmup, samples, || async {
        let _k = Kernel::start(KernelConfig::default());
    })
    .await;
}

// ---- dispatch latency for in-process (no gRPC) handlers --------------------

#[tokio::test]
async fn in_process_dispatch_latency() {
    let kernel = Kernel::start(KernelConfig::default());
    kernel
        .bus()
        .register_handler(
            "bench.echo",
            |inv: Invocation| async move { Ok(inv.payload) },
        )
        .await;

    let warmup = 100;
    let samples = 1000;
    bench_fn("In-process dispatch", warmup, samples, || {
        let bus = kernel.bus().clone();
        async move {
            let _ = bus
                .dispatch(inv("bench.echo", 30_000, vec_payload(128)))
                .await;
        }
    })
    .await;
}

// ---- dispatch through a real gRPC round-trip -------------------------------

#[cfg(feature = "gateway")]
struct EchoPlugin;

#[cfg(feature = "gateway")]
#[tonic::async_trait]
impl ForgePlugin for EchoPlugin {
    async fn register(
        &self,
        _: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        Ok(Response::new(RegisterResponse {
            plugin_protocol_version: "1.0".into(),
            capabilities: vec![Capability {
                name: "bench.grpc".into(),
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
            result: Some(forge::proto::invoke_response::Result::Payload(r.payload)),
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

#[cfg(feature = "gateway")]
#[tokio::test]
async fn grpc_dispatch_latency() {
    let plugin = EchoPlugin;
    let (addr, _sh) = serve(plugin).await;
    let manifest = manifest(
        "bench-grpc",
        &addr,
        PluginLifecycleConfig {
            restart_policy: "never".into(),
            restart_backoff_initial_ms: 200,
            restart_backoff_max_ms: 1000,
            restart_max_attempts: 1,
            health_check_interval_ms: 10_000,
            health_check_failure_threshold: 3,
            drain_grace_period_ms: 50,
        },
    );

    let registry = Registry::new();
    let bus = Bus::new(registry.clone());
    let manager = Manager::new(registry.clone(), bus.clone());
    manager.start_all(vec![manifest]).await;
    assert_eq!(
        manager.plugin_state("bench-grpc").await,
        Some(PluginState::Ready)
    );

    let warmup = 100;
    let samples = 1000;
    bench_fn("gRPC dispatch", warmup, samples, || {
        let bus = bus.clone();
        async move {
            let _ = bus
                .dispatch(inv("bench.grpc", 30_000, vec_payload(128)))
                .await;
        }
    })
    .await;
}

// ---- chained invocations — 1, 5, and 10 hops deep --------------------------

async fn build_chain(kernel: &Kernel, length: usize) {
    for i in 0..length {
        let cap = format!("chain.{i}");
        let bus = kernel.bus().clone();
        kernel
            .bus()
            .register_handler(&cap, move |inv: Invocation| {
                let bus = bus.clone();
                async move {
                    if inv.capability == format!("chain.{}", length - 1) {
                        Ok(inv.payload)
                    } else {
                        let n: usize = inv
                            .capability
                            .rsplit('.')
                            .next()
                            .unwrap_or("0")
                            .parse()
                            .unwrap_or(0);
                        let next = format!("chain.{}", n + 1);
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
}

#[tokio::test]
async fn chained_invocation_1() {
    let kernel = Kernel::start(KernelConfig::default());
    build_chain(&kernel, 1).await;
    let warmup = 10;
    let samples = 100;
    bench_fn("Chained invocation (1 hop)", warmup, samples, || {
        let bus = kernel.bus().clone();
        async move {
            let _ = bus.dispatch(inv("chain.0", 30_000, vec_payload(64))).await;
        }
    })
    .await;
}

#[tokio::test]
async fn chained_invocation_5() {
    let kernel = Kernel::start(KernelConfig::default());
    build_chain(&kernel, 5).await;
    let warmup = 10;
    let samples = 100;
    bench_fn("Chained invocation (5 hops)", warmup, samples, || {
        let bus = kernel.bus().clone();
        async move {
            let _ = bus.dispatch(inv("chain.0", 60_000, vec_payload(64))).await;
        }
    })
    .await;
}

#[tokio::test]
async fn chained_invocation_10() {
    let kernel = Kernel::start(KernelConfig::default());
    build_chain(&kernel, 10).await;
    let warmup = 10;
    let samples = 100;
    bench_fn("Chained invocation (10 hops)", warmup, samples, || {
        let bus = kernel.bus().clone();
        async move {
            let _ = bus.dispatch(inv("chain.0", 60_000, vec_payload(64))).await;
        }
    })
    .await;
}

// ---- how long from start_all to Ready for a fresh plugin? ------------------

#[cfg(feature = "gateway")]
#[tokio::test]
async fn plugin_startup_time() {
    use forge::lifecycle::PluginState;

    struct QuickPlugin;
    #[tonic::async_trait]
    impl ForgePlugin for QuickPlugin {
        async fn register(
            &self,
            _: Request<RegisterRequest>,
        ) -> Result<Response<RegisterResponse>, Status> {
            Ok(Response::new(RegisterResponse {
                plugin_protocol_version: "1.0".into(),
                capabilities: vec![Capability {
                    name: "quick.start".into(),
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
                result: Some(forge::proto::invoke_response::Result::Payload(r.payload)),
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

    let lc = PluginLifecycleConfig {
        restart_policy: "never".into(),
        restart_backoff_initial_ms: 200,
        restart_backoff_max_ms: 1000,
        restart_max_attempts: 1,
        health_check_interval_ms: 10_000,
        health_check_failure_threshold: 3,
        drain_grace_period_ms: 50,
    };

    let warmup = 2;
    let samples = 10;
    let mut result = BenchResult::new("Plugin startup (gRPC)", warmup, samples);

    async fn startup_once(lc: PluginLifecycleConfig) -> u64 {
        let plugin = QuickPlugin;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        let a_str = format!("http://{}:{}", a.ip(), a.port());
        let _serve_h: tokio::task::JoinHandle<()> = tokio::spawn(async move {
            Server::builder()
                .add_service(ForgePluginServer::new(plugin))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(l))
                .await
                .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let m = manifest("bench-startup", &a_str, lc);
        let reg = Registry::new();
        let b = Bus::new(reg.clone());
        let mgr = Manager::new(reg.clone(), b.clone());

        let start = tokio::time::Instant::now();
        mgr.start_all(vec![m]).await;
        let elapsed = start.elapsed().as_nanos() as u64;

        let _ = wait_for_state(
            &mgr,
            "bench-startup",
            PluginState::Ready,
            Duration::from_secs(5),
        )
        .await;
        elapsed
    }

    for _ in 0..warmup {
        startup_once(lc.clone()).await;
    }
    for _ in 0..samples {
        let elapsed = startup_once(lc.clone()).await;
        result.record(elapsed);
    }

    result.report();
}

// ---- how fast are registry lookups with 100 entries? -----------------------

#[tokio::test]
async fn registry_lookup_latency() {
    let registry = Registry::new();
    for i in 0..100 {
        registry.register(
            format!("reg.bench.{i}"),
            semver::Version::new(1, 0, 0),
            forge::registry::PluginHandle {
                plugin_name: format!("p{i}"),
                instance_id: format!("inst{i}"),
            },
        );
    }

    let warmup = 100;
    let samples = 1000;
    bench_fn("Registry lookup", warmup, samples, || {
        let reg = registry.clone();
        async move {
            let _ = reg.lookup("reg.bench.0", &semver::VersionReq::parse("*").unwrap());
        }
    })
    .await;
}

// ---- memory footprint at idle, after kernel start, handlers, and dispatch --

#[tokio::test]
async fn memory_usage() {
    println!("═══ Memory usage ═══");

    // just the tokio runtime, nothing else
    let mem_idle = memory_kb();
    println!(
        "  idle (runtime only):     {:>8} KB ({:.1} MB)",
        mem_idle,
        mem_idle as f64 / 1024.0
    );

    // measure the bump from starting the kernel
    let kernel = Kernel::start(KernelConfig::default());
    let mem_kernel = memory_kb();
    let delta_kernel = mem_kernel.saturating_sub(mem_idle);
    println!(
        "  after Kernel::start:     {:>8} KB ({:.1} MB)  +{} KB",
        mem_kernel,
        mem_kernel as f64 / 1024.0,
        delta_kernel
    );

    // how much does 100 in-process handlers cost?
    for i in 0..100 {
        let bus = kernel.bus().clone();
        kernel
            .bus()
            .register_handler(&format!("mem.{i}"), move |inv: Invocation| {
                let bus = bus.clone();
                async move {
                    let _ = bus;
                    Ok(inv.payload)
                }
            })
            .await;
    }
    let mem_after_register = memory_kb();
    let delta_register = mem_after_register.saturating_sub(mem_kernel);
    println!(
        "  after 100 handlers reg: {:>8} KB ({:.1} MB)  +{} KB",
        mem_after_register,
        mem_after_register as f64 / 1024.0,
        delta_register
    );

    // 1000 dispatches — any un-reclaimed allocations?
    for _ in 0..1000 {
        let _ = kernel
            .bus()
            .dispatch(inv("mem.0", 30_000, vec_payload(128)))
            .await;
    }
    let mem_after_dispatch = memory_kb();
    let delta_dispatch = mem_after_dispatch.saturating_sub(mem_after_register);
    println!(
        "  after 1,000 dispatches:  {:>8} KB ({:.1} MB)  +{} KB",
        mem_after_dispatch,
        mem_after_dispatch as f64 / 1024.0,
        delta_dispatch
    );
}

// ---- crash the plugin, wait for restart — how long does it take? -----------

#[cfg(feature = "gateway")]
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
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    mgr.plugin_state(name).await
}

#[cfg(feature = "gateway")]
#[tokio::test]
async fn restart_latency() {
    use forge::lifecycle::PluginState;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    struct CrashAfterInvoke {
        cap_name: &'static str,
        crashed: Arc<AtomicBool>,
    }

    #[tonic::async_trait]
    impl ForgePlugin for CrashAfterInvoke {
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
                result: Some(forge::proto::invoke_response::Result::Payload(r.payload)),
            }))
        }
        async fn health_check(
            &self,
            _: Request<HealthCheckRequest>,
        ) -> Result<Response<HealthCheckResponse>, Status> {
            if self.crashed.load(Ordering::SeqCst) {
                Err(Status::unavailable("crashed"))
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

    let fast_lc = PluginLifecycleConfig {
        restart_policy: "on-failure".into(),
        restart_backoff_initial_ms: 50,
        restart_backoff_max_ms: 1000,
        restart_max_attempts: 5,
        health_check_interval_ms: 50,
        health_check_failure_threshold: 1,
        drain_grace_period_ms: 50,
    };

    let warmup = 1;
    let samples = 5;
    let mut result = BenchResult::new("Restart latency (crash→Ready)", warmup, samples);

    for iteration in 0..warmup + samples {
        let crashed = Arc::new(AtomicBool::new(false));
        let plugin = CrashAfterInvoke {
            cap_name: "restart.bench",
            crashed: crashed.clone(),
        };
        let (addr, _sh) = serve(plugin).await;
        let m = manifest("restart-bench", &addr, fast_lc.clone());

        let registry = Registry::new();
        let bus = Bus::new(registry.clone());
        let manager = Manager::new(registry.clone(), bus.clone());
        manager.start_all(vec![m]).await;

        let state = wait_for_state(
            &manager,
            "restart-bench",
            PluginState::Ready,
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(state, Some(PluginState::Ready));

        // one dispatch makes it crash (CrashAfterInvoke pattern)
        let _ = bus
            .dispatch(inv("restart.bench", 5000, vec_payload(64)))
            .await;

        // wait until the manager realises it's dead
        let _ = wait_for_state(
            &manager,
            "restart-bench",
            PluginState::Stopped,
            Duration::from_secs(5),
        )
        .await;

        // now time how long it takes to come back from Stopped to Ready
        let ready_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let start_wait = tokio::time::Instant::now();
        let result_state = loop {
            let s = manager.plugin_state("restart-bench").await;
            if s == Some(PluginState::Ready) {
                break s;
            }
            if tokio::time::Instant::now() >= ready_deadline {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let elapsed = start_wait.elapsed().as_nanos() as u64;
        assert_eq!(
            result_state,
            Some(PluginState::Ready),
            "plugin should reach Ready after restart"
        );

        if iteration >= warmup {
            result.record(elapsed);
        }
    }

    result.report();
}

// ---- environment info dump -------------------------------------------------

#[tokio::test]
async fn environment_info_test() {
    println!("{}", environment_info());
}
