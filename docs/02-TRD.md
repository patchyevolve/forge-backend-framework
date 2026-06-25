# Forge — Technical Requirements Document

**Document 2 of 7 — Foundational**
**Status:** Final
**Depends on:** Document 1 (PRD)
**Read before:** Documents 3, 4, 5

---

## 1. Purpose

The PRD says *what* Forge is and *why*. This document fixes the *hard* technical decisions — the ones that are expensive to reverse once Document 3 (Architecture) and onward are written on top of them. Every choice here includes the reasoning, because "no mistakes" requires that the reasoning survive, not just the conclusion.

---

## 2. Language: Rust (core), language-agnostic (plugins)

**Decision:** The kernel is implemented entirely in Rust, stable toolchain, no nightly features in the shipped product.

**Reasoning:**

- **FFI surface.** Rust can export a stable C ABI (`extern "C"` + `#[no_mangle]`) without a runtime to drag along — no GC, no green-thread scheduler that needs to be initialized by the host. This is what makes "embed Forge inside a C, C++, or anything-with-a-C-FFI program" actually feasible, which directly serves PRD §6's "library/crate" consumption model.
- **Static, dependency-free binaries.** `cargo build --release` against the `x86_64-unknown-linux-musl` target produces a single statically linked executable. This directly satisfies PRD §6's "downloadable normal executable" requirement — no shared libraries, no runtime installation step for the operator.
- **Memory safety without GC pauses.** Matters for the embedded use case (PRD §7) and for predictable latency in the daemon use case. You already operate in this paradigm (OPERtur, mapit) — Forge stays consistent with your existing toolchain investment rather than introducing a second systems language.
- **async ecosystem maturity.** Tokio is the de facto standard for async Rust, has first-class support in `tonic` (gRPC) and `axum`/`hyper` (HTTP), and is what the "fully async, max throughput" decision (locked in your requirements) needs underneath it.

**What plugins are written in:** anything. This is enforced precisely because the plugin boundary is a *wire protocol* (Document 4), not a Rust trait. A plugin process can be a Python script, a Go binary, a C program, a Node service — the kernel never compiles or links against plugin code. This is the mechanism that makes the PRD's "any language asking for backend work" promise literal rather than aspirational.

---

## 3. Execution Model: Fully Async (Tokio)

**Decision:** The kernel runtime is a multi-threaded Tokio runtime. All kernel-internal I/O (gateway listeners, plugin connections, internal bus) is async. Blocking work, if a plugin needs it, is the plugin's own problem to solve (e.g., via `spawn_blocking` if the plugin happens to be Rust) — the kernel never blocks its own runtime threads.

**Reasoning:** You explicitly chose this over sync/blocking simplicity, accepting the added mental-model complexity for throughput. Architecturally this means:

- The kernel must never call a blocking syscall on a Tokio worker thread. Any spot that needs one (e.g., reading a manifest file at startup) uses `tokio::fs` or is confined to startup-before-runtime-is-serving-traffic.
- Plugin connections (Document 4) are async streams; a slow or hung plugin must not stall the executor — enforced via per-connection timeouts (Document 3 §6).
- Internal message passing uses `tokio::sync::mpsc` / `broadcast` channels, never OS threads with blocking queues, for the kernel-internal bus (Document 3 §4).

---

## 4. Wire Protocol: gRPC + HTTP, Both, by Design (Not as a Compromise)

**Decision:** Forge's gateway speaks **both** gRPC and HTTP/JSON simultaneously, on separate listeners, against the *same* underlying capability registry. Neither is "the real one with the other bolted on."

**Reasoning, and when each is used:**

| | gRPC | HTTP/JSON |
|---|---|---|
| Used for | plugin↔kernel registration & invocation (internal), and performance-sensitive external clients | external/operator-facing traffic, ad hoc testing, curl-ability, and any plugin author who doesn't want to deal with protobuf codegen |
| Why | typed contracts via `.proto`, codegen for effectively every language (satisfies "bindable from any language" with compile-time safety), streaming support, low overhead | zero-codegen entry point — universal, debuggable with curl, lowest barrier for a quick plugin or a quick demo |
| Cost | requires protoc + generated stubs per language | higher per-message overhead, no native streaming semantics without extra work |

Both protocols are translated, at the gateway boundary, into the *same* internal invocation representation (Document 3 §5) before reaching the registry. A plugin registers its capabilities once; whether a caller reaches it via gRPC or HTTP is a gateway-layer detail the plugin never has to know about. This is the concrete mechanism behind PRD §4 principle 2 ("the protocol is the product") — there are two encodings of one contract, not two contracts.

**Consequence for Document 4:** the canonical protocol definition is the `.proto` file. The HTTP/JSON surface is a documented, deterministic transcoding of the same messages (field names match, JSON uses standard protobuf JSON mapping per `google.protobuf` conventions), not a separately hand-maintained REST API.

---

## 5. Kernel Minimality: Microkernel, Not Hybrid

**Decision:** Locked from your answer — the kernel ships with **no built-in routing, auth, persistence, or serialization logic**. The kernel's only first-class responsibilities are:

1. Process & connection lifecycle for plugins (start, health-check, stop, restart policy)
2. The capability registry (what can each connected plugin do, keyed by versioned capability names)
3. The internal async message bus (routing an invocation from gateway → correct plugin → response back)
4. Configuration loading (manifest files, environment, CLI flags)
5. The dual gRPC/HTTP gateway (Document 4's transport layer)

Everything else — including the "official" routing plugin, the "official" auth plugin, the "official" SQLite/Postgres persistence plugin — ships in a separate `forge-plugins-official` collection, versioned and released independently of the kernel. They are privileged only in the sense that they're maintained alongside the kernel and documented as recommended defaults; they are not privileged in the code — they talk to the kernel through the exact same protocol any third-party plugin uses. This is the literal test of microkernel purity: **if removing the official plugins requires touching kernel code, the design has failed.**

**Reasoning:** This is the only design that satisfies PRD §4 principle 4 (degrade toward zero) for the embedded case while still satisfying the full web-backend case, because it makes "what's loaded" purely a function of what plugins you point Forge at, never a function of what code path is compiled into the kernel.

---

## 6. Performance & Resource Budgets

These are targets, not guarantees, but they are the numbers Document 3's design is accountable to:

- **Kernel idle footprint** (zero plugins loaded, gateway listening, no traffic): target under 10 MB resident memory on x86-64 Linux. This is the number that has to look plausible for the embedded narrative.
- **Cold start to "ready" (gateway accepting connections):** under 50ms on a typical dev machine with zero plugins; under 500ms with the official plugin set loaded.
- **Per-invocation overhead added by the kernel** (gateway-decode → registry-lookup → bus-dispatch → plugin, excluding the plugin's own work and network RTT to an out-of-process plugin): target under 1ms p99 for in-process/loopback plugins.
- **Plugin crash isolation:** a single plugin process crashing must not increase kernel memory usage beyond releasing that plugin's registry entries and connection state — no leaked channels, no leaked task handles. (Verified via the failure-injection tests referenced in Document 3 §6.)

These numbers exist so that later performance work has a target to fall short of or beat, rather than "make it fast" being unfalsifiable.

---

## 7. Versioning & Compatibility Policy

This is the concrete mechanism behind PRD §4 principle 5.

- **Semantic Versioning** (`MAJOR.MINOR.PATCH`) applies independently to: (a) the kernel binary/crate, (b) the wire protocol (the `.proto` definitions), (c) the plugin manifest schema. These three version numbers are tracked separately because they evolve at different rates — you can patch the kernel without touching the protocol, but you cannot change the protocol without it being visible to every plugin author.
- **Protocol compatibility rule:** within a MAJOR protocol version, the kernel guarantees it can talk to any plugin built against any MINOR/PATCH of the same MAJOR. New fields are always added as optional (proto3 default behavior already gives you this — never break it by making a new field required). A plugin built against protocol `1.0` must still register successfully against a kernel speaking protocol `1.9`.
- **Manifest schema versioning:** every manifest file declares `forge_manifest_version: "1.0"` explicitly (Document 4 §3). The kernel refuses to load a manifest whose major version it doesn't understand, with a clear error — it never guesses.
- **Breaking changes require a MAJOR bump and a migration note** in `CHANGELOG.md`, full stop, no exceptions, including for "this is just an internal cleanup" changes if they touch the public protocol or manifest schema.

---

## 8. Security Posture (baseline, expanded in Documents 4 and 6)

- Plugins are **trusted code you chose to load** — Forge v-final does not implement plugin sandboxing, capability-based permission restriction, or signature verification. This is stated explicitly so it is never assumed. (A WASM-based sandboxed plugin runtime is identified as credible future work, not a v-final requirement — seePRD §5.2 boundary discipline.)
- All loopback/local communication (Unix domain sockets, where used — Document 4 §2) defaults to filesystem-permission-based access control (socket file mode `0600`).
- Network-exposed gateway listeners (when Forge is run as an external-facing daemon) support TLS termination via standard Rust TLS crates (`rustls`); plaintext is the default for local development and is loudly logged as such on startup, never silent.
- No telemetry, no phone-home, no required network call at runtime beyond what an operator explicitly configures (PRD §6 success criterion 7). This matters for the security-tool showcase use case directly.

---

## 9. Explicit Non-Requirements (carried forward from PRD §5.2, restated as technical constraints so they aren't reintroduced by accident)

- No requirement to support `no_std` for the kernel itself in v-final (the *protocol* must remain implementable from a `no_std` plugin context, e.g. an ESP32 talking raw gRPC/HTTP over WiFi — but the kernel binary itself assumes a hosted OS).
- No requirement for Windows-native testing; WSL2 compatibility is best-effort and documented as such.
- No requirement for horizontal clustering / multi-node coordination. Forge is one kernel process per node; multi-node backend topologies are an application-level concern built using multiple Forge instances, not a kernel feature.

---

## 10. Toolchain & Dependency Baseline

Locking these here so Document 5 (build spec) and Document 7 (plugin guide) reference one source of truth rather than drifting:

| Component | Choice | Why |
|---|---|---|
| Language/toolchain | Rust stable (via `rustup`) | matches your existing setup; no nightly-only features |
| Async runtime | `tokio` (multi-thread) | industry standard, required by `tonic`/`axum` |
| gRPC | `tonic` + `prost` | the standard Rust gRPC stack, async-native |
| HTTP | `axum` | built on `hyper`/Tokio, ergonomic routing for the gateway's own (kernel-level, not plugin-level) endpoints |
| Serialization | Protocol Buffers (wire canonical) + `serde_json` (HTTP transcoding) | one schema, two encodings, per §4 |
| Dynamic plugin loading (in-process, Rust-only optional path) | `libloading` | documented as an *optional* fast-path for same-language plugins; never the only path (§5, §6 of Document 4) |
| TLS | `rustls` | pure Rust, avoids OpenSSL system-dependency friction, consistent with static-binary goal |
| Target triple (primary) | `x86_64-unknown-linux-gnu` (dev), `x86_64-unknown-linux-musl` (distribution) | matches your Fedora/Ryzen workstation; musl gives the static binary |

---

## 11. Forward Reference

Document 3 (Architecture Specification) takes every decision above as fixed input and answers *how the kernel is internally structured* to deliver on them. Document 4 (Plugin Protocol Specification) takes §4 and §7 above and turns them into the literal `.proto` files and manifest schema.
