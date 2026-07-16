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
├── Cargo.toml                      # workspace root
├── crates/
│   ├── forge-core/                  # kernel logic: lifecycle, registry, bus, config
│   ├── forge-proto/                 # generated Rust stubs from the .proto
│   ├── forge-gateway/               # gRPC + HTTP listeners
│   ├── forge-cli/                   # the `forge` binary
│   └── forge-plugin-sdk-rust/       # optional convenience crate for Rust plugin authors
├── proto/
│   └── forge_plugin_v1.proto        # canonical protobuf definition
├── plugins-official/                # official plugins (auth example, etc.)
└── examples/                        # example projects
```

The workspace boundary between `forge-core` and `forge-gateway` is deliberate: an embedder who only needs the registry/bus/lifecycle logic depends on `forge-core` alone and never pulls in `forge-gateway`'s listeners.

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
forge-core = "1.0"
```

```rust
use forge_backend::kernel::{Kernel, KernelConfig};

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
- Starter plugins (auth, health, example, calculator)
- `docker-compose.yml`, `.gitignore`, `README.md`

## 4. The forge-core Public API

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
| `gateway-grpc` | on (forge-cli), off (forge-core) | tonic gRPC listener |
| `gateway-http` | on (forge-cli), off (forge-core) | axum HTTP listener |
| `metrics` | off | Prometheus `/metrics` endpoint |
| `tls` | on | rustls TLS termination |

An embedder who wants zero transport code:

```toml
forge-core = { version = "1.0", default-features = false }
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
