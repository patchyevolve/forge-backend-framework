# Forge

**A polyglot backend microkernel** — spawn, manage, and route requests between plugins written in any language that speaks gRPC.

Forge is not an HTTP framework, not a service mesh, and not an event bus. It is a thin lifecycle kernel that connects plugin processes together through a capability-based message bus. Plugins register what they can do; the kernel routes invocations to the right plugin, regardless of language.

## Architecture

```
┌──────────────┐      ┌─────────────────────────────────────┐
│   HTTP/gRPC   │ ──→  │            Forge Kernel              │
│    Ingress    │      │  ┌─────────┐  ┌──────────────────┐  │
│  (Gateway)    │      │  │ Registry │  │       Bus        │  │
│               │      │  │ (what)   │  │ (resolve→deadline│  │
│  curl, SDKs,  │      │  │          │  │  →dispatch→reply)│  │
│  browsers     │      │  └─────────┘  └──────────────────┘  │
└──────────────┘      │         │              │             │
                      │    ┌────┴──────────────┘             │
                      │    │  ┌──────────────┐               │
                      │    └─→│   Manager    │               │
                      │       │ (health,     │               │
                      │       │  restart,    │               │
                      │       │  lifecycle)  │               │
                      │       └──────────────┘               │
                      └─────────────────────────────────────┘
                                  │
                    ┌─────────────┼─────────────┐
                    ▼             ▼             ▼
              ┌──────────┐ ┌──────────┐ ┌──────────┐
              │ echo-rs  │ │ auth-jwt │ │  Python  │
              │  (Rust)  │ │  (Rust)  │ │  plugin  │
              └──────────┘ └──────────┘ └──────────┘
```

## Quick start

```bash
# download the binary
curl -fsSL https://raw.githubusercontent.com/patchyevolve/forge-backend-framework/master/install.sh | sh

# run with the example backend
forge run --config examples/example-backend/forge.toml
```

## Performance

| Operation | Latency (p50) |
|---|---|
| In-process dispatch | 4.7 µs |
| gRPC dispatch | 117 µs |
| Registry lookup | 1.5 µs |
| Plugin startup | 1 ms |
| Restart after crash | 55 ms |

Benchmarked on AMD Ryzen 7 7735HS, release mode, `tonic` feature enabled.

## What it is not

- **Not a web framework** — Forge does not route HTTP. Plugins do that.
- **Not a service mesh** — no sidecars, no proxies, no mTLS between plugins.
- **Not an event bus** — no pub/sub, no queues, no streaming (yet).

## Languages

| Plugin | SDK | Status |
|---|---|---|
| Rust | `forge-plugin-sdk-rust` | Stable |
| Python | Example (`plugin.py`) | Reference |

## Status

v1.0.0 — API stable. See [CHANGELOG.md](CHANGELOG.md).

## Specification

Nine design documents covering every design decision:

- [Product Requirements](docs/01-PRD.md)
- [Technical Requirements](docs/02-TRD.md)
- [Architecture Specification](docs/03-architecture-spec.md)
- [Plugin Protocol](docs/04-plugin-protocol-spec.md)
- [Build & Distribution](docs/05-build-distribution-spec.md)
- [Operator's Guide](docs/06-operators-guide.md)
- [Plugin Developer's Guide](docs/07-plugin-developers-guide.md)
- [Build Order Addendum](docs/08-BUILD-ORDER-ADDENDUM.md)
- [Verification & Release](docs/09-VERIFICATION-AND-RELEASE.md)
