# Forge — Build & Distribution Specification

**Document 5 of 7 — Core Design**
**Status:** Final
**Depends on:** Documents 1, 2, 3, 4
**Read before:** Document 6

---

## 1. Purpose

PRD §5.1 commits to three distribution shapes from one codebase. This document specifies the actual crate layout, build commands, and packaging steps that deliver all three without divergent forks of the logic.

---

## 2. Crate Layout

```
forge/
├── Cargo.toml                  # workspace root
├── crates/
│   ├── forge-core/             # the kernel logic itself: lifecycle, registry, bus, config (Architecture Spec §2.1-2.4)
│   │                            #   — depends on NOTHING gateway/transport-specific; pure async logic + the public
│   │                            #     embedding API (see §4 below). This is what someone embeds as a library.
│   ├── forge-proto/             # generated Rust stubs from the canonical .proto (Document 4 §5), plus the .proto file itself
│   ├── forge-gateway/            # gRPC + HTTP listeners (Architecture Spec §2.5) — depends on forge-core + forge-proto
│   ├── forge-cli/                 # the `forge` binary: CLI parsing, calls into forge-core + forge-gateway
│   └── forge-plugin-sdk-rust/      # optional convenience crate for Rust-language plugin authors (thin wrapper
│                                    #   over forge-proto's generated client/server code) — NOT required to write
│                                    #   a Rust plugin, purely ergonomic sugar (Document 7 §[Rust quickstart])
├── proto/
│   └── forge_plugin_v1.proto    # the canonical file from Document 4 §5 — single source of truth, forge-proto builds from this
├── plugins-official/             # forge-plugins-official collection (Document 2 §5) — versioned and released
│   ├── forge-plugin-http-router/    #   SEPARATELY from the kernel, deliberately not in the same release cadence
│   ├── forge-plugin-auth-jwt/
│   └── forge-plugin-data-sqlite/
└── examples/
    └── example-backend/          # the PRD §5.1 example backend assembled from plugins-official
```

The workspace boundary between `forge-core` and `forge-gateway` is the literal embodiment of PRD §5.1's distinction between "library/crate" consumers and "daemon" consumers — someone who wants Forge embedded with their *own* transport layer (e.g., already has an `axum` app and just wants the registry/bus/lifecycle logic inside it) depends on `forge-core` alone and never pulls in `forge-gateway`'s listeners.

---

## 3. The Three Distribution Shapes, Concretely

### 3.1 Shape 1 — Installable Executable

```bash
# what an operator runs (Document 6 covers this from the user side)
curl -fsSL https://example.invalid/install.sh | sh
```

The install script (kept deliberately simple and auditable — operators should be able to read it in under a minute before piping it to `sh`, especially for the security-tool showcase audience):

1. Detects OS + arch (`uname -s`, `uname -m`).
2. Downloads the matching prebuilt static binary from the release artifacts (built per §3.4 below) — `forge-x86_64-unknown-linux-musl`, etc.
3. Verifies a SHA-256 checksum published alongside the release (manual, documented step — no signing infrastructure in v-final, per PRD §5.2).
4. Places the binary at `~/.local/bin/forge` (or `/usr/local/bin/forge` if run as root), matching the convention `rustup`/similar installers use, since that's a pattern you're already comfortable navigating.

This binary **is** `forge-cli`, statically linked against `forge-core` and `forge-gateway` — there is no separate runtime to install.

### 3.2 Shape 2 — Rust Crate (Embedding)

```toml
# a third party's own Cargo.toml
[dependencies]
forge-core = "1.0"
# forge-gateway only if they want Forge's own transport instead of bringing their own
```

```rust
// the literal ~20-line embedding example PRD §6 success criterion 3 requires
use forge_core::{Kernel, KernelConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = KernelConfig::from_file("forge.toml")?;
    let kernel = Kernel::start(config).await?;

    // kernel.registry() and kernel.bus() are now usable directly from the
    // embedding program — e.g. a security tool could call
    // kernel.bus().dispatch(invocation) itself, never touching forge-gateway
    // at all, satisfying the PRD §7 security-tool showcase use case.

    kernel.run_until_shutdown().await?;
    Ok(())
}
```

This is published to crates.io in the literal sense the crate is *structured* to be publishable (proper `Cargo.toml` metadata, no workspace-only path dependencies leaking into the public API) — per PRD §5.1, actually publishing it is optional/cosmetic, not a hard requirement, but nothing about the structure should make publishing it later any harder than `cargo publish`.

### 3.3 Shape 3 — Standalone Daemon

This is `forge-cli` run directly, talked to over its gRPC/HTTP gateway (Architecture Spec §2.5) by anything — including non-Rust, non-plugin external callers (a frontend, a curl script, a monitoring tool). This is Shape 1's binary, just used in long-running-service mode rather than as a one-shot local install:

```bash
forge run --config /etc/forge/forge.toml
# or, systemd-managed (Document 6 covers the unit file)
```

No separate build artifact is required for this shape — it's the same binary as Shape 1, used differently. This is deliberate: PRD §6 success criteria treat "downloadable executable" and "daemon" as different *usage modes* of one artifact, not different builds, which is the simplest possible answer to "three distribution shapes from one codebase."

### 3.4 Cross-Compilation & Release Artifacts

```bash
# the actual release build commands, run in CI (or manually) per target
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl -p forge-cli

# resulting static binary:
target/x86_64-unknown-linux-musl/release/forge
```

Verify the binary has no dynamic dependencies beyond the kernel itself (confirms the static-link goal from TRD §2):

```bash
ldd target/x86_64-unknown-linux-musl/release/forge
# expected output: "not a dynamic executable" (musl static link)
```

Primary release target per TRD §10: `x86_64-unknown-linux-musl`. `aarch64-unknown-linux-musl` is a documented secondary target (covers ARM-based dev boards, relevant to the embedded narrative in §7 below) using the same `rustup target add` + `cargo build --target` pattern.

---

## 4. The `forge-core` Public Embedding API (Surface Contract)

Per Document 1 §6 success criterion 3, this surface must be small and stable. The public API a Shape 2 consumer depends on is intentionally limited to:

```rust
pub struct Kernel { /* ... */ }
pub struct KernelConfig { /* ... */ }

impl Kernel {
    pub async fn start(config: KernelConfig) -> Result<Self, KernelError>;
    pub fn registry(&self) -> &Registry;
    pub fn bus(&self) -> &Bus;
    pub async fn run_until_shutdown(self) -> Result<(), KernelError>;
    pub async fn shutdown(self) -> Result<(), KernelError>;
}

impl KernelConfig {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ConfigError>;
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError>;
    pub fn builder() -> KernelConfigBuilder; // programmatic construction, no file needed
}
```

`Registry` and `Bus` expose the operations specified in Architecture Spec §2.2 and §2.3 directly (`lookup`, `dispatch`, `list_capabilities`, etc.) — there is no separate "embedding-friendly" subset; the embedding API *is* the same registry/bus the gateway itself uses internally, which is the whole reason embedding is even sound — an embedder gets the real thing, not a simplified shim that drifts from what the daemon shape actually does.

---

## 5. Build-Time Feature Flags

To support PRD §5.2's exclusion of a built-in UI and TRD §5's microkernel purity without forcing every consumer to compile code they'll never use:

| Feature flag | Default | Effect |
|---|---|---|
| `gateway-grpc` | on (for `forge-cli`), off (for bare `forge-core`) | compiles the `tonic`-based gRPC listener |
| `gateway-http` | on (for `forge-cli`), off (for bare `forge-core`) | compiles the `axum`-based HTTP listener |
| `metrics` | off | compiles Prometheus-style `/metrics` endpoint support (Document 6 §[observability]) |
| `tls` | on | compiles `rustls`-based TLS termination support (TRD §8) |

A Shape 2 embedder who wants zero transport code at all (bringing their own, e.g. wiring `bus::dispatch` into an existing `axum` app they already run) depends on `forge-core` with all gateway features disabled — `forge-core = { version = "1.0", default-features = false }` — and pulls in nothing from `forge-gateway` whatsoever.

---

## 6. Reproducibility

`Cargo.lock` is committed (binary-shape project, not a library meant to float on caller's dependency resolution for the `forge-cli`/`forge-gateway` crates — though `forge-core` and `forge-plugin-sdk-rust`, being libraries other people's projects will depend on, follow normal semver-range dependency practice in their own `Cargo.toml` and are not pinned by a committed lockfile of their own). Release builds are built from a clean `cargo clean && cargo build --release --locked` to guarantee the published binary matches exactly what `Cargo.lock` describes — `--locked` causes the build to fail rather than silently re-resolve if the lockfile and manifests ever drift, which is the property that matters for a security-conscious audience verifying what they're running.

---

## 7. Embedded / Constrained-Target Profile

Per PRD §7's embedded showcase and TRD §9's explicit `no_std`-kernel non-requirement: the **kernel itself** (the thing running `forge-core`/`forge-gateway`/`forge-cli`) is not intended to run *on* the constrained device (an ESP32-class microcontroller cannot host a Tokio multi-threaded runtime or a `musl` Linux binary — that's not a realistic target for the kernel process). The embedded story instead works like this, and this framing is itself the deliverable for that showcase:

- A constrained device runs a **plugin** — not the kernel — communicating with a Forge kernel running on a normal host (a Raspberry Pi, a laptop, a server) over the network, using the HTTP/JSON on-ramp from Document 4 §7 (simplest to implement on a microcontroller's network stack — no protobuf codegen needed on-device) or raw gRPC if the device's toolchain supports it (e.g., via `esp-idf`'s networking stack plus a minimal handwritten protobuf encoder, which is a credible stretch goal connecting directly to your existing esp-rs self-study track).
- This is "Shape A, plugin-as-server" (Document 4 §2) running on the device, dialed into by a kernel elsewhere — or, if the device is behind NAT/can't accept inbound connections, a documented variant where the device polls/long-polls the kernel's HTTP endpoint instead, noted here as an accepted gap: the core protocol (Document 4) assumes the kernel can reach the plugin or vice versa over a normal socket, and NAT traversal is explicitly not solved by Forge itself.
- The architectural point being demonstrated: **the protocol (Document 4), not the kernel binary, is what scales down to embedded** — exactly per TRD §9's distinction between "no `no_std` kernel required" and "the protocol must remain implementable from a `no_std` context." This is the load-bearing claim of the whole embedded narrative, and it should be stated exactly this way if this project comes up in an interview: Forge doesn't run *on* the microcontroller, the microcontroller *participates in* a Forge system as a peer speaking the same wire contract anything else speaks.

---

## 8. Forward Reference

Document 6 picks up from the installed/built artifacts this document produces and walks an operator through actually running them. Document 7 picks up from the protocol this enables and walks a plugin author through targeting it in a real language.
