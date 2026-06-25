# Forge — Architecture Specification

**Document 3 of 7 — Core Design**
**Status:** Final
**Depends on:** Documents 1 (PRD), 2 (TRD)
**Read before:** Documents 4, 5

---

## 1. Purpose

This document describes how the Forge kernel is structured *internally* — the modules, their responsibilities, and how a request actually moves through the system from arrival to response. Document 4 then takes the boundary this document defines (the registry + bus interface) and specifies the literal wire format plugins use to cross it.

If you (or an agent extending Forge later) ever need to answer "where does X live," this document is the source of truth. If X doesn't fit cleanly into one of the modules below, that's a signal X is plugin territory, not kernel territory — re-check PRD §4 principle 1 before adding it here.

---

## 2. The Five Kernel Modules

Per TRD §5, the kernel has exactly five areas of responsibility. Each gets its own module (Rust crate-internal module, not necessarily a separate crate, though `forge-core` is structured so each *could* be split into its own crate later without an API change):

```
forge-core/
├── lifecycle/      — plugin process & connection lifecycle
├── registry/        — the capability registry
├── bus/              — internal async message routing
├── config/           — manifest + config loading
└── gateway/          — gRPC + HTTP transport
```

### 2.1 `lifecycle`

Owns the state machine for every connected plugin. A plugin, from the kernel's point of view, is always in exactly one of these states:

```
DISCOVERED → CONNECTING → HANDSHAKING → READY → DEGRADED → DRAINING → STOPPED
                                            ↑________________|
                                         (health check recovery)
```

- **DISCOVERED**: the kernel has read a manifest (Document 4 §3) referencing this plugin but has not yet attempted to connect.
- **CONNECTING**: kernel has initiated a transport connection (spawned the process and/or dialed its declared socket/address).
- **HANDSHAKING**: transport connected; kernel and plugin are exchanging the registration handshake (Document 4 §4) — capability declaration, protocol version negotiation.
- **READY**: handshake succeeded; the plugin's declared capabilities are now live in the registry and may receive invocations.
- **DEGRADED**: a health check (Document 4 §6) failed once; the plugin remains in the registry (so in-flight assumptions don't break instantly) but new invocations are not routed to it until it recovers or exceeds the failure threshold.
- **DRAINING**: kernel is shutting the plugin down (operator command, manifest reload, or repeated health-check failure past threshold) — no new invocations are routed, in-flight ones are allowed to finish up to a configured grace period.
- **STOPPED**: connection closed, capabilities removed from the registry, process (if kernel-managed) terminated.

**Why a state machine and not just "connected/disconnected":** the DEGRADED state is what makes TRD §6's "plugin crash isolation" target achievable without being binary/brutal — a flaky plugin gets a chance to recover before being cut off, but the registry never routes traffic to something known-unhealthy. This state machine is the single source of truth `registry` consults before routing any invocation.

### 2.2 `registry`

A concurrent, versioned map from **capability name** → **plugin handle + invocation metadata**. This is the heart of the microkernel — it is the *only* place "what can the system currently do" is knowable.

A capability is declared by a plugin during handshake as a tuple:

```
(capability_name: String, version: SemVer, input_schema_ref, output_schema_ref, plugin_handle)
```

Example capability names from the official plugin set (Document 7 has full examples): `forge.http.route`, `forge.auth.verify`, `forge.data.query`. Capability names are namespaced by convention (`<domain>.<area>.<verb>`) but the kernel does not enforce or interpret the namespace — it is an opaque string key as far as the registry is concerned. This matters: **the kernel has zero built-in knowledge of what "auth" or "routing" mean.** It only knows that some plugin claims to provide a string-keyed capability at some version.

Registry operations:

- `register(capability, plugin_handle)` — called only by `lifecycle` on transition into READY.
- `deregister(capability, plugin_handle)` — called on transition into STOPPED, or DRAINING-grace-period-expiry.
- `lookup(capability, version_constraint) -> Option<PluginHandle>` — called by `bus` for every invocation. Version constraints follow Cargo-style SemVer requirement syntax (`^1.2`, `=1.0.0`, etc.) so a caller can pin or float.
- `list_capabilities() -> Vec<CapabilitySummary>` — read-only introspection, what powers `forge status` (Document 6) and any operator tooling.

**Concurrency model:** the registry is implemented as a `tokio::sync::RwLock` (or an equivalent lock-free structure, e.g. `dashmap`, if profiling under TRD §6's targets shows lock contention — this is an implementation choice the architecture leaves open, but the *interface* above is fixed) guarded map. Reads (lookups, which dominate at runtime) do not block each other; writes (register/deregister, which only happen at plugin lifecycle transitions, a comparatively rare event) are serialized.

**Multiple plugins, same capability:** the registry permits this by design (it's how you'd run two instances of a data plugin for sharding, or A/B two implementations). Resolution policy (first-registered wins, round-robin, explicit routing rules) is **not** a kernel decision — it's exposed as a pluggable resolution strategy, with "first-ready wins" as the shipped default, consistent with PRD §4 principle 1.

### 2.3 `bus`

The internal async router. Every invocation — regardless of whether it arrived via the gRPC gateway, the HTTP gateway, or another plugin calling another plugin — becomes one `Invocation` value:

```rust
struct Invocation {
    capability: String,
    version_constraint: VersionReq,
    payload: Bytes,           // protobuf-encoded, canonical form per TRD §4
    metadata: HeaderMap,       // trace id, deadline, caller identity if any
    deadline: Instant,
}
```

`bus::dispatch(invocation) -> Result<Bytes, InvocationError>` does exactly four things, in order:

1. Ask `registry` to resolve `capability` + `version_constraint` to a live `PluginHandle` (in `READY` state only).
2. Enforce the `deadline` — if it's already passed, short-circuit with `InvocationError::DeadlineExceeded` without touching the plugin.
3. Send `payload` + `metadata` down that plugin's connection (a `tokio::sync::mpsc` channel feeding the plugin's actual transport, Document 4 §5), and await the response, racing against the deadline via `tokio::time::timeout`.
4. Return the plugin's response, or a typed `InvocationError` (`NotFound`, `DeadlineExceeded`, `PluginUnhealthy`, `TransportError`, `PluginError(code, message)` — the last one carrying a structured error the *plugin itself* produced, passed through verbatim).

**Why this is the architecture's only "routing" logic, and why it's still not a router:** `bus` does not know what an HTTP route is. It dispatches by opaque capability name. The fact that `forge.http.route` capabilities happen to carry path/method matching logic is entirely internal to the routing *plugin* — the plugin registers itself once for a broad capability and does its own sub-dispatch internally, or registers many fine-grained capabilities (e.g. `forge.http.route./users.GET`) and lets the kernel's plain string-equality lookup do the work. Both are valid; Document 7 shows both patterns. The kernel is agnostic to which one a given routing plugin chooses.

### 2.4 `config`

Loads, in this precedence order (highest wins): CLI flags → environment variables (`FORGE_*` prefix) → manifest file(s) → built-in defaults. Responsible for:

- Parsing the top-level `forge.toml` (kernel config: gateway bind addresses, TLS settings, log level, plugin directory paths) — distinct from plugin manifests (Document 4 §3), which describe individual plugins, not the kernel itself.
- Discovering plugin manifests (by directory scan or explicit list) and handing them to `lifecycle` to begin the DISCOVERED state.
- Validating `forge_manifest_version` and `forge.toml`'s own schema version against what this kernel build understands, per TRD §7 — refusing to start with a clear error on mismatch rather than guessing.

### 2.5 `gateway`

The only module that speaks to the outside world over a network socket. Hosts two listeners:

- **gRPC listener** (`tonic`-based) — implements the service defined in Document 4's `.proto` file. This is also the protocol plugins themselves use to connect *to* the kernel (a plugin connecting to register is, from the transport's point of view, just another gRPC client — see Document 4 §2 for why this unifies "external caller" and "plugin" into one transport model).
- **HTTP listener** (`axum`-based) — exposes the same capability surface via the documented JSON transcoding (TRD §4), plus a small set of kernel-native endpoints not backed by any plugin: `/healthz`, `/status` (registry introspection), `/metrics` (if the metrics feature is enabled — Document 6 §[observability]).

`gateway` is a thin translation layer only: it decodes an incoming gRPC/HTTP request into an `Invocation` (§2.3) and hands it to `bus::dispatch`. It contains **no business logic** — it does not know what any capability does, only how to get bytes off the wire and into the canonical `Invocation` shape and back.

---

## 3. Request Lifecycle, End to End

Concrete walkthrough, referenced by Document 7 when explaining how a plugin author's code actually gets reached:

1. An HTTP `GET /users/42` arrives at `gateway`'s HTTP listener.
2. `gateway` has no built-in notion of `/users/42` — it consults `registry::list_capabilities()` filtered to capabilities of a well-known kernel-level *meta*-capability, `forge.http.route` (this one name is the sole hardcoded string in the gateway, and even it is just a registry lookup key, not special-cased dispatch logic), to find which registered plugin claims to handle this path/method. (How a routing plugin advertises "I own `GET /users/{id}`" is a plugin-level convention documented in Document 7 §[routing plugin pattern], not a kernel concern — the kernel only ever sees one opaque capability key per registered route, however the routing plugin chooses to mint that key.)
3. `gateway` builds an `Invocation { capability: "forge.http.route.users.get", payload: <encoded request>, deadline: now + configured_timeout, .. }` and calls `bus::dispatch`.
4. `bus` asks `registry::lookup("forge.http.route.users.get", "*")`, gets back the routing plugin's handle, checks the deadline, and forwards the payload over that plugin's connection.
5. The routing plugin (in whatever language it's written) does its own logic — which here happens to be: call `forge.auth.verify` (another `bus::dispatch`, this time initiated *by the plugin*, not the gateway — plugins are full bus clients, not just bus targets) to check the caller's token, then call `forge.data.query` to fetch user 42, then format a response.
6. The routing plugin's final response bytes flow back up through `bus::dispatch`'s awaited future, back through `gateway`, encoded as an HTTP response, sent to the original caller.

The critical architectural property visible here: **steps 5's two sub-invocations are indistinguishable, from the bus's perspective, from the original gateway-initiated invocation in step 3.** There is no special "gateway invocation" type and "plugin-to-plugin invocation" type — there is exactly one `Invocation` shape and one dispatch path, used uniformly. This is what principle 1 (mechanism, not policy) looks like concretely at the request-flow level, and it's why adding a brand-new kind of caller later (e.g., a scheduled-job runner) requires zero kernel changes — it just needs to be able to construct an `Invocation` and call `bus::dispatch`, exactly like the gateway does.

---

## 4. The Internal Bus, Concretely

Per TRD §3, the bus is built on Tokio channels, not OS threads. Each `READY` plugin connection owns:

- An outbound `mpsc::Sender<PluginRequest>` (kernel → plugin)
- A mechanism to correlate responses back to the right `bus::dispatch` caller — a `oneshot::Sender<PluginResponse>` stashed in a `DashMap<RequestId, oneshot::Sender<..>>` keyed by a kernel-generated request ID, resolved when the plugin's response (carrying the same ID) arrives on its inbound stream.

This request-ID-correlation pattern (rather than assuming strict request/response ordering on the connection) is required because a single plugin connection carries *concurrent* invocations — the routing plugin in the walkthrough above has its own response to the gateway still pending while it issues two more invocations itself, all potentially interleaved with traffic from a completely unrelated caller hitting the same plugin at the same time.

---

## 5. Failure Semantics (the contract TRD §6 holds the implementation to)

This is written as a contract precisely so behavior here is never "whatever the code happens to do":

- **A plugin process crashes:** `lifecycle` detects this (process exit, or transport disconnect for kernel-unmanaged plugins) and transitions it directly to `STOPPED`, synchronously deregistering every capability it provided. Any `bus::dispatch` call mid-flight to that plugin resolves with `InvocationError::TransportError` immediately — it does not hang until a timeout. In-flight calls *the crashed plugin itself had made outward* (step 5's sub-invocations) are themselves separate `Invocation`s with their own deadlines and are unaffected by the crash of the *original* caller; they complete or time out independently, because the bus has no notion of a "call tree" to unwind, only individual invocations. (If callers want call-tree cancellation, that's a pattern built using the `metadata` trace-id field at the application/plugin level, not a kernel guarantee.)
- **A plugin hangs (process alive, not responding):** the health-check loop (Document 4 §6) on a configurable interval (default 5s) sends a lightweight ping; missing N consecutive pings (default 3) transitions `READY → DEGRADED`. New invocations stop being routed to it (the registry lookup in step 4 simply skips DEGRADED handles). In-flight invocations already dispatched to it still respect their own `deadline` and fail with `DeadlineExceeded` on expiry, not earlier — the kernel doesn't preemptively cancel them on a health-check failure alone, since that would be a policy decision (PRD §4 principle 1) about how tolerant to be, which belongs in the lifecycle's configurable thresholds, not hardcoded behavior.
- **The kernel itself panics:** Forge follows the standard Rust convention — a panic in a `tokio::spawn`ed task (e.g., one handling a single connection) is caught by Tokio's task boundary and only kills that task, logged loudly; it does not bring down the whole kernel process. A panic during kernel startup (before the runtime is serving) is fatal and exits non-zero with the panic message, which is correct — there is nothing useful to keep running.
- **Restart policy:** `lifecycle` supports per-plugin restart policies declared in the manifest (Document 4 §3): `never`, `on-failure` (with backoff), `always`. Default is `on-failure` with exponential backoff capped at 30s, 5 attempts, then the plugin is left in `STOPPED` permanently until an operator intervenes (`forge plugin restart <name>`, Document 6).

---

## 6. Hot Reload (Plugins Only — Kernel Hot Reload Is Explicitly Out of Scope, Per PRD §5.2)

A manifest change detected at runtime (file-watch on the plugin directory, opt-in via `forge.toml`) drives a plugin through `READY → DRAINING → STOPPED`, followed by a fresh `DISCOVERED → ... → READY` cycle for the updated manifest/binary — using the exact same state machine as a cold start, not a special "reload" code path. This is a direct consequence of the state machine in §2.1 being the *only* way plugin state changes — there is no shortcut that bypasses DRAINING, which is what guarantees in-flight requests to the old version finish cleanly (within the grace period) rather than being cut off mid-response.

---

## 7. What This Architecture Deliberately Does Not Decide

Per PRD §4 principle 1, the following are explicitly plugin-territory and this document takes no position on their internal design — they appear here only to be ruled *out* of the kernel, so a future reader doesn't go looking for them in `forge-core`:

- How a routing plugin parses path templates or matches HTTP methods.
- How an auth plugin validates a token (JWT, session lookup, mTLS — kernel doesn't care).
- How a data plugin talks to its backing store, or whether it has one at all.
- Whether two plugins agree on any shared data format beyond the bare `Invocation.payload: Bytes` — if `forge.http.route` and `forge.data.query` need to agree on a "User" shape, that agreement is a convention between those two plugins (documented at the *plugin* level, e.g. in `forge-plugins-official`'s own docs), not something the kernel schema-checks.

This boundary is the single most important thing for a future contributor (human or agent) to internalize before touching `forge-core` — and is restated explicitly here so Document 7 can point back to it whenever a plugin author asks "can I just add this to the kernel instead."
