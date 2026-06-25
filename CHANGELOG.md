# Changelog

## v1.0.0 (2026-06-25)

Initial release of **Forge** — a polyglot backend microkernel.

### Architecture
- **forge-backend** (published as `forge-backend` on crates.io): lifecycle (7-state `PluginState`), concurrent `Registry` (DashMap), `Bus` (4-step dispatch: resolve → deadline → send+await → return), config loader (`ConfigLoader` + `ForgeConfig` + `PluginManifest`), embedding API (`Kernel` + `KernelConfig`)
- **forge-proto**: canonical plugin protocol (`forge_plugin_v1.proto`) with Register, Invoke, HealthCheck, Drain RPCs. Builds via tonic + prost-build with vendored protoc.
- **forge-gateway**: gRPC + HTTP (axum) ingress translation layer with UUID request ID at ingress, healthz/status/invoke/restart endpoints
- **forge-cli**: `forge run`, `forge status` (text + graph), `forge plugin restart`, file watcher for hot-reload
- **forge-plugin-sdk-rust**: `Plugin` trait, `PluginServer` (Shape A/B), `KernelClient` (outbound invoke), `InvokeContext`
- **Official plugins**: echo-rs, auth-jwt, data-sqlite, http-router (with request-ID propagation)
- **Python SDK**: echo-py plugin (Shape B, foreign-language interop)

### Key features
- In-process and gRPC dispatch with deadline enforcement (checked at top of `dispatch()`)
- Crash detection → backoff-based restart coordinator (channel-based to break async type cycles)
- Exponential backoff with configurable initial/max/attempts; crash-driven restarts accumulate; operator restarts reset
- Health check loop with configurable interval and failure threshold; initial settle delay prevents first-tick race
- Capability registry with `FirstReadyWins` and `RoundRobin` resolution strategies
- `#[non_exhaustive]` on all public enums for forward compatibility
- `#[must_use]` on constructors and lookup APIs
- Feature-gated tonic dependency (`default = ["tonic"]`; `--no-default-features` for embedded/minimal builds)
- Offline build support (Cargo.lock committed, no network at build time)
- Release packaging via `install.sh` + `release-targets.toml` with SHA-256 verification

### Performance (release mode, x86_64)
| Metric | Latency (p50) | Throughput |
|---|---|---|
| In-process dispatch | 4.7 µs | 210,722 req/s |
| gRPC dispatch | 117.0 µs | 8,214 req/s |
| Registry lookup | 1.5 µs | 617,101 req/s |
| Kernel startup | 3.4 µs | — |
| Plugin startup | 1.05 ms | — |
| Restart (crash→Ready) | 55 ms | — |
| Chained invocation (10 hops) | 19.6 µs | — |

### Test coverage
- 24 unit tests (config, lifecycle, registry, bus)
- 3 lifecycle integration tests (gRPC round-trips)
- 8 failure injection scenarios (hang, corrupt, crash, never-healthy, broken-connection, restart-storm, re-crash isolation, multi-failure isolation)
- 9 stress scenarios (1,000 concurrent, 200 chained, kill-under-load, restart-loops, large payloads, 10,000 sequential, deadline hammering, registry contention, simultaneous reg/dereg)
- 11 performance benchmarks (dispatch, startup, lookup, memory, chained, restart)
- CI matrix (lint, test, build, publish-dry-run, offline-build, integration)

### Release artifacts
- `forge-x86_64-unknown-linux-musl.tar.gz` (primary, CI-built)
- `forge-aarch64-unknown-linux-musl.tar.gz` (secondary, documented)
- `install.sh` — curl-pipe-sh installer with OS/arch detection and SHA-256 verification
