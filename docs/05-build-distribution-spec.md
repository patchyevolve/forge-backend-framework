# Forge Build & Distribution Specification

## 1. Purpose

Forge delivers three distribution shapes from one codebase:

| Shape | What | Who uses it |
|---|---|---|
| **Shape 1** | Installable binary | Operators running `forge` as a daemon |
| **Shape 2** | Rust crate | Developers embedding the kernel in their own Rust program |
| **Shape 3** | Workspace + CLI | Plugin developers building and running Forge projects |

## 2. Crate Layout

```
forge-core/
├── Cargo.toml              # workspace root
├── forge/                   # monolithic library crate (kernel + gateway + proto + SDK)
│   ├── Cargo.toml
│   ├── build.rs             # protobuf codegen via tonic-build
│   └── src/
│       ├── lib.rs           # feature-gated modules
│       ├── bus.rs           # dispatch, invocation, error types
│       ├── config.rs        # TOML config loading
│       ├── kernel.rs        # Kernel embedding API
│       ├── lifecycle.rs     # PluginState + Manager
│       ├── proto.rs         # generated protobuf stubs
│       ├── registry.rs      # capability registry (DashMap-based)
│       ├── sdk.rs           # Plugin trait, PluginServer, KernelClient
│       └── gateway/         # gRPC + HTTP listeners (feature = "gateway")
│           ├── mod.rs
│           ├── grpc.rs
│           └── http.rs
├── cli/                     # the `forge` binary crate
│   ├── Cargo.toml
│   └── src/main.rs
├── proto/
│   └── forge_plugin_v1.proto
├── plugins-official/        # official plugins (auth, data, router)
├── examples/                # example projects and tutorials
└── systemd/
    └── forge.service        # systemd unit for daemon deployment
```

The `gateway` feature flag is off by default when using `forge` as a library — an embedder who only needs the registry/bus/lifecycle logic depends on `forge` without the `gateway` feature and never pulls in axum/tonic listeners.

## 3. The Three Distribution Shapes

### 3.1 Shape 1 — Installable Binary

Build from source:

```bash
git clone https://github.com/patchyevolve/forge-backend-framework
cd forge-core
cargo build --release -p forge-cli
sudo cp target/release/forge /usr/local/bin/
```

Verify:

```bash
forge --version
# forge 1.0.0
```

### 3.2 Shape 2 — Rust Crate (Embedding)

```toml
# Cargo.toml
[dependencies]
forge = { version = "1.0", default-features = false }
```

```rust
use forge::kernel::{Kernel, KernelConfig};

let config = KernelConfig::from_file("forge.toml")?;
let kernel = Kernel::start(config);

// Use kernel.registry() and kernel.bus() directly
let result = kernel.bus().dispatch(Invocation::simple(
    "app.health", b"".to_vec()
)).await?;
```

The embedding API (`Kernel`, `Registry`, `Bus`) is the same code the gateway uses internally — no simplified shim.

### 3.3 Shape 3 — Workspace + CLI

This is the standard developer workflow:

```bash
forge init my-project
cd my-project
cargo build --release
forge run
```

`forge init` creates:
- A Cargo workspace with plugin crates
- `forge/forge.toml` with routes and plugin definitions
- Starter plugins (auth, health, example)
- `docker-compose.yml`, `.gitignore`, `README.md`

## 4. The forge Public API

```rust
pub struct Kernel { /* ... */ }
pub struct KernelConfig { /* ... */ }

impl Kernel {
    pub fn start(config: KernelConfig) -> Self;
    pub fn registry(&self) -> &Registry;
    pub fn bus(&self) -> &Bus;
}

impl KernelConfig {
    pub fn from_file(path: &str) -> Result<Self, anyhow::Error>;
}

impl Registry {
    pub fn lookup(&self, name: &str) -> Option<PluginHandle>;
    pub fn list_capabilities(&self) -> Vec<CapabilitySummary>;
}

impl Bus {
    pub fn dispatch(&self, invocation: Invocation) -> Result<Bytes, InvocationError>;
}
```

## 5. Build-Time Feature Flags

| Feature | Default | Effect |
|---|---|---|
| `gateway` | on (default), off (library) | axum HTTP listener + tonic gRPC listener |
| `sdk` | on (default), off (library) | `Plugin` trait, `PluginServer`, `KernelClient` |

An embedder who wants zero transport code:

```toml
forge = { version = "1.0", default-features = false }
```

## 6. Reproducibility

`Cargo.lock` is committed. Release builds use:

```bash
cargo build --release --locked
```

The `--locked` flag causes the build to fail rather than silently re-resolve if the lockfile and manifests drift — critical for reproducible releases.

## 7. Cross-Compilation

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl -p forge-cli

# Verify static link
ldd target/x86_64-unknown-linux-musl/release/forge
# expected: "not a dynamic executable"
```

Primary target: `x86_64-unknown-linux-musl`. Secondary: `aarch64-unknown-linux-musl`.
