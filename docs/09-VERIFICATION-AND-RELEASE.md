# Forge — Verification & Release

**Document 9 of 9 — Release Readiness**
**Status:** Final
**Depends on:** Documents 1–8 (this is the validation layer over the entire system)
**Audience:** Release managers, CI maintainers, and anyone verifying the implementation matches the specification

---

## 1. Verification Matrix

Every major feature and the test(s) that exercise it:

| Feature | Test(s) | Location |
|---|---|---|
| Configuration loading | `config::tests::default_config_loads`, `parse_valid_forge_toml`, `manifest_dir_resolved_against_config_dir` | `crates/forge-core/src/config/mod.rs` |
| Environment variable overrides | `config::tests::env_overrides_apply_string`, `env_overrides_apply_bool`, `env_overrides_do_not_apply_when_unset`, `load_config_uses_env_overrides` | `crates/forge-core/src/config/mod.rs` |
| Manifest version validation | `config::tests::reject_unsupported_manifest_version` | `crates/forge-core/src/config/mod.rs` |
| Manifest discovery | `config::tests::discover_no_manifest_dir`, `manifest_dir_resolved_against_config_dir` | `crates/forge-core/src/config/mod.rs` |
| Registry — register/lookup | `registry::tests::register_and_lookup`, `lookup_nonexistent_capability`, `version_mismatch_returns_none` | `crates/forge-core/src/registry/mod.rs` |
| Registry — deregister | `registry::tests::deregister_plugin` | `crates/forge-core/src/registry/mod.rs` |
| Registry — list | `registry::tests::list_capabilities` | `crates/forge-core/src/registry/mod.rs` |
| Lifecycle state machine | `lifecycle::tests::happy_path`, `full_lifecycle`, `health_check_recovery`, `illegal_stopped_to_ready`, `reentry_from_stopped`, `draining_path` | `crates/forge-core/src/lifecycle/mod.rs` |
| Bus — dispatch to missing capability | `bus::tests::dispatch_to_nonexistent_plugin` | `crates/forge-core/src/bus/mod.rs` |
| Bus — deadline enforcement | `bus::tests::deadline_already_past` | `crates/forge-core/src/bus/mod.rs` |
| Lifecycle — real gRPC connection | `lifecycle_integration::lifecycle_connects_to_plugin_and_routes_invocation` | `crates/forge-core/tests/lifecycle_integration.rs` |
| Lifecycle — drain and shutdown | `lifecycle_integration::shutdown_calls_drain_on_plugin` | `crates/forge-core/tests/lifecycle_integration.rs` |
| Lifecycle — restart state machine | `lifecycle_integration::restart_state_machine` | `crates/forge-core/tests/lifecycle_integration.rs` |
| Crash detection + restart | `test_kill_resilience.sh` (6 phases) | `test_kill_resilience.sh` |
| Full committed-backend chain | `test_committed_backend.sh` (8 tests) | `test_committed_backend.sh` |
| Request ID propagation | Verified in `test_committed_backend.sh` output (UUID traces across router→auth→data) | End-to-end log inspection |
| Embedding API | `examples/embedded-minimal/` (19-line crate) | `examples/embedded-minimal/` |
| Offline build + run | `test_offline_build.sh` (strace-verified zero connect calls) | `test_offline_build.sh` |
| Minimal build profile | Manual: `cargo build -p forge-core --no-default-features` (zero warnings) | `crates/forge-core/Cargo.toml` |
| CLI — run, status, restart | `test_committed_backend.sh` exercises all three | `crates/forge-cli/src/main.rs` |
| CLI — status --graph | Manual: `forge status --graph` parses manifests without running kernel | `crates/forge-cli/src/main.rs` |
| File-watch hot reload | Manual: `[plugins] watch = true` polls manifests every 3s | `crates/forge-cli/src/main.rs` |
| Protocol versioning | `Register` handshake in `lifecycle_integration.rs` exercises proto compat | Proto definition at `crates/forge-proto/proto/` |

---

## 2. Build Matrix

The following commands must all pass before any release:

```bash
# Formatting — no deviations from `rustfmt` defaults
cargo fmt --check

# Linting — zero warnings at `-D warnings` level
cargo clippy --all-targets -- -D warnings

# Unit + integration tests (27 current, including 3 real-gRPC lifecycle tests)
cargo test

# Production build — no dev-only dependencies leaked
cargo build --release -p forge-cli

# Minimal/embedded profile — no tonic required
cargo build -p forge-core --no-default-features

# Offline build proof — all crate sources already cached by `cargo fetch`
cargo build --offline

# Dry-run publish — confirm all metadata is crates.io-compatible
cargo package -p forge-proto
cargo package -p forge-plugin-sdk-rust
cargo package -p forge-core

# The following crates are intentionally not publishable:
#   forge-gateway — internal transport layer
#   forge-cli     — binary, not a library
```

### Expected outcomes

| Command | Expected result |
|---|---|
| `cargo fmt --check` | Exit 0, no output |
| `cargo clippy --all-targets -- -D warnings` | Exit 0, zero warnings |
| `cargo test` | 27 passed, 0 failed |
| `cargo build --release -p forge-cli` | Single static binary at `target/release/forge` |
| `cargo build -p forge-core --no-default-features` | Clean compile, 0 warnings |
| `cargo build --offline` | Clean compile, 0 network `connect()` syscalls |
| `cargo package -p forge-proto` | `.crate` tarball with `proto/` included |
| `cargo package -p forge-plugin-sdk-rust` | `.crate` tarball, `path` deps stripped to version |
| `cargo package -p forge-core` | `.crate` tarball, `path` dep on `forge-proto` correctly versioned |

---

## 3. Runtime Validation

Tests that require a running kernel or process orchestration:

| Test | What it proves | How to run |
|---|---|---|
| **kill-resilience** (`test_kill_resilience.sh`) | Crash detection, deregistration, `TransportError` propagation, restart with backoff, no kernel panic | `bash test_kill_resilience.sh` |
| **committed backend** (`test_committed_backend.sh`) | Full 4-plugin chain: http-router → auth-jwt → data-sqlite + echo-rs; request ID propagation; `/healthz`, `/v1/status`, `/v1/invoke`, `/v1/plugins/{name}/restart` | `bash test_committed_backend.sh` |
| **embedded example** (`examples/embedded-minimal/`) | 19-line embedding API works: `Kernel` + `register_handler` + `dispatch` | `cargo run --example embedded-minimal` |
| **installer** (`install.sh`) | OS/arch detection, SHA-256 verification, `~/.local/bin` placement | Manual: `bash install.sh --help` |
| **offline boot** (`test_offline_build.sh`) | `cargo fetch` + `cargo build --offline` + `forge run` with zero network calls | `bash test_offline_build.sh` |
| **hot reload** | `[plugins] watch = true` polls and restarts on manifest change | Manual: edit a `.toml` while forge is running with `watch = true` |
| **CLI graph** (`forge status --graph`) | Dependency graph renders `provides`/`requires` from manifests | `forge status --graph` against any config with plugin manifests |

---

## 4. Release Checklist

```markdown
- [ ] All items in §2 Build Matrix pass, confirmed by CI (or manual run for pre-CI)
- [ ] All items in §3 Runtime Validation pass
- [ ] §1 Verification Matrix — every feature check is green
- [ ] Documentation synchronized:
      - docs/03-architecture-spec.md
      - docs/04-plugin-protocol-spec.md
      - docs/06-operators-guide.md
      - docs/07-plugin-developers-guide.md
      - docs/08-build-order-addendum.md
      - docs/09-verification-and-release.md
- [ ] Version updated:
      - Root Cargo.toml `[workspace.package] version`
      - All crate-level Cargo.toml files (workspace `version` inherits)
- [ ] CHANGELOG.md updated with summary of changes since last release
- [ ] Release artifacts generated:
      - `forge-x86_64-unknown-linux-musl.tar.gz`
      - `forge-aarch64-unknown-linux-musl.tar.gz`
      - SHA-256 checksum files for each artifact
- [ ] Installer verified against release artifacts:
      - `bash install.sh` downloads, checksums, and installs correctly
- [ ] Protocol manifest (`.proto`) checked for breaking changes
      - MAJOR bump only if existing field removed or required field added
      - MINOR bump for additive-only changes
```

---

## 5. Known Limitations

Intentionally documented limitations — not bugs, but design choices or deferred work:

| Limitation | Where | Reason |
|---|---|---|
| **RoundRobin resolution is a stub** | `crates/forge-core/src/registry/mod.rs:116-119` | `FirstReadyWins` covers the shipped use case; full round-robin requires per-capability index tracking. Documented inline with a `//` comment. |
| **File watching uses polling (3s interval)** | `crates/forge-cli/src/main.rs:111-129` | Polling avoids a dependency on platform-specific notification APIs (inotify, kqueue, FSEvents). Adequate for development and moderate-scale deployments. |
| **Crash detection by `tonic::Code::Unavailable`** | `crates/forge-core/src/lifecycle/manager.rs:372` | The current implementation treats any gRPC `Unavailable` error as a crash. A plugin that restarts its gRPC listener without changing its address will be briefly (≤1 health-check interval) misidentified as dead. This is within spec — TRD §6 permits transient misdetection within the health-check budget. |
| **Plugin processes are not sandboxed** | All `ManagedSubprocess` transport paths | Forge's threat model (TRD §8) explicitly assumes plugins are trusted code. No OS-level sandboxing (seccomp, Landlock, SELinux, container isolation) is applied. If sandboxing is required, it must be configured externally (systemd unit hardening, container runtime, etc.). |
| **Metrics endpoint is documented but not implemented** | `docs/06-operators-guide.md:334-343` | The `/metrics` endpoint and Prometheus counters are defined in the Operator's Guide as a forward-looking reference. The `metrics` build feature exists in the distribution spec but is not wired in the current gateway code. A production deployment should add a metrics middleware before relying on this endpoint. |

---

## 6. Version 1.0 Statement

The following are considered stable interfaces for Forge v1.0. A breaking change to any of these constitutes a MAJOR version bump:

### 6.1 Public Rust APIs

- `forge_core::bus::{Bus, Invocation, InvocationError, HandlerFn, PluginConnection}`
- `forge_core::registry::{Registry, PluginHandle, CapabilitySummary, ResolutionStrategy}`
- `forge_core::lifecycle::{PluginState, Manager}` (Manager is gated behind `feature = "tonic"`)
- `forge_core::config::{ForgeConfig, ConfigLoader, PluginManifest, PluginLifecycleConfig, PluginCapabilitiesDecl, DiscoveredPlugin}`
- `forge_core::kernel::{Kernel, KernelConfig}`
- `forge_proto::{InvokeRequest, InvokeResponse, RegisterRequest, RegisterResponse, Capability, HealthCheckRequest, HealthCheckResponse, DrainRequest, DrainResponse, PluginError}`
- `forge_plugin_sdk::{Plugin, PluginServer, KernelClient, InvokeContext, InvokeResult}`

### 6.2 Protocol Compatibility

- The `.proto` service definition (`forge.plugin.v1.ForgePlugin`) is stable at protocol version `1.0`
  - Adding new RPC methods to the service is a MINOR bump
  - Removing or renaming an existing field in any message is a MAJOR bump
  - Adding optional fields to any message is a MINOR bump
- The handshake (`Register` RPC) version negotiation uses `plugin_protocol_version` / `kernel_protocol_version` — matching MAJOR is required for compatibility

### 6.3 Manifest Schema

- `forge_manifest_version` = `"1.x"` — stable
- All fields in `PluginManifest` (§3 of Document 4) are stable at `1.0`
- Adding a new optional section/field to the manifest schema is a MINOR bump
- Removing or renaming an existing field is a MAJOR bump

### 6.4 Configuration Format

- `forge_config_version` = `"1.x"` — stable
- All sections in `[gateway]`, `[log]`, `[plugins]` are stable at `1.0`
- Environment variable overrides (`FORGE_*` prefix) are stable at `1.0`

### 6.5 CLI Commands

| Command | Status |
|---|---|
| `forge run --config <path>` | Stable |
| `forge status` | Stable |
| `forge status --graph` | Stable |
| `forge plugin restart <name>` | Stable |
| `forge --version` | Stable |
| `forge --help` | Stable |

---

## 7. CI Integration

---

## 8. Forward Work — Tiered Roadmap

### Tier 1 — Release Blockers (highest value)

**CI/CD (GitHub Actions):**
```yaml
jobs:
  lint:
    steps:
      - cargo fmt --check
      - cargo clippy --all-targets -- -D warnings

  test:
    steps:
      - cargo test

  build:
    strategy:
      matrix:
        profile: [release, no-default-features]
    steps:
      - cargo build --profile ${{ matrix.profile == 'release' && '--release' || '' }}
        ${{ matrix.profile == 'no-default-features' && '-p forge-core --no-default-features' || '-p forge-cli' }}

  publish-dry-run:
    steps:
      - cargo package -p forge-proto
      - cargo package -p forge-plugin-sdk-rust
      - cargo package -p forge-core

  offline-build:
    steps:
      - cargo fetch
      - cargo build --offline

  integration:
    steps:
      - bash test_committed_backend.sh
      - bash test_offline_build.sh
```

**Publishability:**
- `cargo package --all`
- `cargo publish --dry-run`
  Catches missing files, bad manifests, broken build scripts.

**SemVer/API review — ensure public APIs are stable before publishing 1.0:**
- `forge-core`
- `forge-plugin-sdk-rust`
- `forge-proto`

### Tier 2 — Production Hardening

**Stress testing:**
- Hundreds/thousands of concurrent requests
- Kill plugins repeatedly during load
- Rapid restart loops
- Large payloads
- Deadline expiration

**Failure injection:**
- Slow plugins
- Corrupted protobuf
- Half-open sockets
- Plugin never answers health checks
- Manager restart storms

**Performance benchmarking:**
- Dispatch latency
- Throughput
- Startup time
- Memory usage
- Embedded mode overhead

### Tier 3 — Developer Experience

**API documentation:**
- Rustdoc on every public type
- Examples for every public API
- `cargo doc --document-private-items`

**More examples:**
- HTTP plugin
- Streaming plugin
- Auth plugin
- Multi-plugin dependency
- Embedded kernel
- Custom capability

### Tier 4 — Nice-to-have

- Replace polling watcher with `notify`
- True `RoundRobin` implementation
- Metrics endpoint
- Distributed tracing (OpenTelemetry)
- Plugin sandboxing
