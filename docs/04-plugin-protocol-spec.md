# Forge — Plugin Protocol Specification

**Document 4 of 7 — Core Design**
**Status:** Final — Protocol Version 1.0
**Depends on:** Documents 1, 2, 3
**Read before:** Documents 5, 7 (this is the document plugin authors need most)

---

## 1. Purpose

This is the literal, implementable contract between the Forge kernel and any plugin, in any language. Document 3 described the registry and bus *conceptually* ("a plugin declares capabilities during handshake"); this document specifies exactly what bytes go on the wire to make that true. If this document and the Rust implementation ever disagree, **this document is correct and the implementation has a bug** — per TRD §2 principle 2.

A plugin author who has read only this document (and Document 7's worked examples) should be able to write a working Forge plugin in a language that has never heard of Rust.

---

## 2. Transport: Why Plugins Are Just gRPC Clients (and Sometimes Servers)

There are two valid transport shapes for a plugin, and the manifest (§3) declares which one a given plugin uses:

**Shape A — Plugin-as-server (recommended default).** The plugin process listens on a Unix domain socket (or TCP port, for non-local plugins) and implements the `ForgePlugin` gRPC service (§5). The kernel, on `CONNECTING`, dials *into* the plugin. This is the recommended shape because it means the plugin's lifecycle (when it's listening, when it shuts down) is entirely the plugin's own responsibility — the kernel never needs to know the plugin's internals, only its address.

**Shape B — Plugin-as-managed-subprocess.** The kernel spawns the plugin as a child process (manifest declares the executable + args) and the plugin, once started, dials *out* to a kernel-provided callback address (passed via an environment variable, `FORGE_CALLBACK_ADDR`) and implements the same `ForgePlugin` service, just initiating the connection in the other direction. This shape exists for plugins that don't want to manage their own listen-socket lifecycle (simple scripts, short-lived utility plugins) and lets the kernel fully own process lifecycle (so `lifecycle`'s restart policy, Document 3 §5, can actually kill and restart the OS process, not just the connection).

Both shapes implement the *identical* `ForgePlugin` gRPC service definition (§5) — the only difference is who dials whom. A plugin author picks based on whether they want the kernel managing their process (Shape B) or not (Shape A). This dual-shape design is what makes Document 1 §5.1's "at least two languages, including one without a managed process model" requirement satisfiable cleanly — a Python plugin run as a one-off script fits Shape B naturally; a long-running Go service fits Shape A naturally.

**Why gRPC even for plugin-as-server local sockets:** consistency. TRD §4 already committed to gRPC as the canonical wire format; reusing the *exact same* protobuf service definition for plugin↔kernel as for external-caller↔kernel (via the gateway, Document 3 §2.5) means there is exactly one schema in the whole system, never two. A plugin and an external gRPC client are, structurally, doing the same kind of conversation with the kernel.

---

## 3. The Manifest

Every plugin is described by a manifest file, `plugin.forge.toml`, which `config` (Document 3 §2.4) discovers and hands to `lifecycle`. This is the only artifact the kernel reads to know a plugin exists — there is no other registration mechanism.

```toml
# plugin.forge.toml — Forge Plugin Manifest
# This file's own schema is versioned independently of the kernel and protocol,
# per TRD §7. A kernel that doesn't understand this major version refuses to
# load it with an explicit error rather than guessing.

forge_manifest_version = "1.0"

[plugin]
name = "example-data-sqlite"        # unique within this Forge instance; used in logs, `forge status`, restart commands
version = "0.3.1"                   # the PLUGIN's own version — independent of forge_manifest_version and protocol version
description = "SQLite-backed persistence plugin (reference implementation)"
protocol_version = "1.0"            # which major.minor of the ForgePlugin protocol this plugin speaks (TRD §7 compat rule applies)

[transport]
shape = "server"                    # "server" (Shape A) or "managed-subprocess" (Shape B)
# --- if shape = "server" ---
address = "unix:///run/forge/plugins/example-data-sqlite.sock"
# --- if shape = "managed-subprocess" ---
# executable = "/usr/bin/python3"
# args = ["-m", "example_data_sqlite"]
# working_dir = "/opt/forge-plugins/example-data-sqlite"

[lifecycle]
restart_policy = "on-failure"       # "never" | "on-failure" | "always" — Document 3 §5
restart_backoff_initial_ms = 500
restart_backoff_max_ms = 30000
restart_max_attempts = 5
health_check_interval_ms = 5000
health_check_failure_threshold = 3
drain_grace_period_ms = 10000

[capabilities]
# Declared here for operator visibility (`forge status` can show this without
# connecting); the AUTHORITATIVE capability list is still whatever the plugin
# actually declares during the live handshake (§4) — this section is advisory
# metadata, not a substitute for the handshake. A mismatch is logged as a
# warning, never treated as an error (the live handshake always wins).
provides = ["forge.data.query@1.0", "forge.data.write@1.0"]
requires = []                        # capabilities this plugin expects to be able to CALL (informational + used by `forge status --graph`)

[env]
# Arbitrary key-value pairs injected into the plugin's environment if
# shape = "managed-subprocess". Ignored for shape = "server".
DATABASE_PATH = "./data/example.db"
```

**Why `provides`/`requires` exist in the manifest when §4's handshake is authoritative:** purely for operator ergonomics — `forge status --graph` (Document 6) can draw the capability dependency graph across all *discovered* plugins before any of them have actually connected, which is useful for diagnosing a misconfigured deployment before it's even running. It is explicitly advisory, restated from the table above, so no implementation is ever tempted to skip the live handshake as an optimization.

---

## 4. The Handshake (`HANDSHAKING` state, concretely)

Upon transport connection (either dial direction, §2), the kernel calls the plugin's `Register` RPC (§5) exactly once:

```
Kernel → Plugin:  RegisterRequest {
                     kernel_protocol_version: "1.0",
                     instance_id: "<uuid, unique per Forge process start>",
                   }

Plugin → Kernel:  RegisterResponse {
                     plugin_protocol_version: "1.0",
                     capabilities: [
                       Capability {
                         name: "forge.data.query",
                         version: "1.0.0",
                         input_schema_ref: "example_data_sqlite.v1.QueryRequest",
                         output_schema_ref: "example_data_sqlite.v1.QueryResponse",
                       },
                       ...
                     ],
                   }
```

- The kernel checks `plugin_protocol_version` against TRD §7's compatibility rule (same MAJOR required; kernel must understand the plugin's MAJOR, plugin's MINOR may exceed what the kernel has seen before — those are presumed-additive fields it can ignore). On incompatibility: kernel logs a clear error, transitions the plugin straight to `STOPPED`, never to `READY`.
- On success, kernel calls `registry::register` once per entry in `capabilities`, and transitions the plugin to `READY`.
- `input_schema_ref`/`output_schema_ref` are **informational strings**, not kernel-enforced types — per Architecture Spec §7, the kernel does not schema-check payloads between two plugins. They exist so `forge status` and plugin-author tooling can display what a capability expects, and so two plugin authors integrating with each other have a documented contract to code against (typically a shared `.proto` file the two plugins agree to import, entirely outside the kernel's involvement).

---

## 5. The `ForgePlugin` Service Definition (Canonical — `.proto`)

This is the literal schema. Anything in Documents 3, 6, or 7 that describes kernel↔plugin communication is describing calls against this service.

```protobuf
syntax = "proto3";
package forge.plugin.v1;

// Implemented by every plugin, regardless of language or transport shape (§2).
service ForgePlugin {
  // Called once, immediately after transport connection. See §4.
  rpc Register(RegisterRequest) returns (RegisterResponse);

  // Called by the kernel to deliver an invocation to this plugin for one of
  // its declared capabilities. This is the ONLY RPC used for actual request
  // traffic — there is no per-capability RPC method, by design (Architecture
  // Spec §2.3: the kernel dispatches by opaque capability name, not by method
  // name, so adding a new capability never requires a new RPC method or a
  // kernel redeploy).
  rpc Invoke(InvokeRequest) returns (InvokeResponse);

  // Lightweight liveness probe. See §6.
  rpc HealthCheck(HealthCheckRequest) returns (HealthCheckResponse);

  // Sent once when the kernel begins draining this plugin (Architecture Spec
  // §2.1, DRAINING state). Plugin should stop initiating NEW outbound
  // invocations but may finish in-flight work. Purely advisory — the kernel
  // does not wait for an acknowledgment beyond the configured grace period.
  rpc Drain(DrainRequest) returns (DrainResponse);
}

// --- Register ---

message RegisterRequest {
  string kernel_protocol_version = 1;
  string instance_id = 2;
}

message RegisterResponse {
  string plugin_protocol_version = 1;
  repeated Capability capabilities = 2;
}

message Capability {
  string name = 1;              // e.g. "forge.data.query" — opaque to the kernel
  string version = 2;           // SemVer, e.g. "1.0.0"
  string input_schema_ref = 3;  // informational only, see §4
  string output_schema_ref = 4; // informational only, see §4
}

// --- Invoke ---

message InvokeRequest {
  string request_id = 1;        // kernel-generated, for correlation (Architecture Spec §4)
  string capability = 2;        // which declared capability is being invoked
  bytes payload = 3;            // capability-specific payload, opaque to the kernel
  map<string, string> metadata = 4;  // trace_id, deadline_unix_ms, caller identity if any
}

message InvokeResponse {
  string request_id = 1;        // echoes the request for correlation
  oneof result {
    bytes payload = 2;          // success: capability-specific response, opaque to kernel
    PluginError error = 3;      // failure: structured error, passed through to the original caller verbatim
  }
}

message PluginError {
  string code = 1;               // plugin-defined error code, e.g. "NOT_FOUND", "VALIDATION_FAILED"
  string message = 2;            // human-readable
  map<string, string> details = 3;
}

// --- HealthCheck ---

message HealthCheckRequest {}

message HealthCheckResponse {
  bool healthy = 1;
  string detail = 2;  // optional free-text, surfaced in `forge status`
}

// --- Drain ---

message DrainRequest {
  uint32 grace_period_ms = 1;  // mirrors the manifest's drain_grace_period_ms
}

message DrainResponse {}
```

This file is the actual artifact you `protoc`-compile for every language a plugin will be written in. Document 7 §[per-language quickstarts] shows the generated-stub usage for Rust and Python concretely; the same file works unmodified for Go, C++, Java, Node, or anything else `protoc` targets.

---

## 6. Health Checking

The kernel's `lifecycle` module calls `HealthCheck` on every `READY` or `DEGRADED` plugin every `health_check_interval_ms` (manifest, default 5000ms). A non-response within a short fixed timeout (default 2000ms, not separately configurable — health checks are meant to be cheap and fast, a slow health check is itself a failure signal) counts as a failed check identically to an explicit `healthy: false` response.

`health_check_failure_threshold` (default 3) consecutive failures triggers `READY → DEGRADED` (Architecture Spec §2.1). A single subsequent success transitions `DEGRADED → READY` immediately — recovery is not similarly debounced, on the principle that it's safer to err toward routing traffic to a plugin that just proved it's responsive than to needlessly prolong degradation.

---

## 7. HTTP/JSON Transcoding (the Non-gRPC On-Ramp)

Per TRD §4, the kernel's HTTP gateway listener exposes the *same* `InvokeRequest`/`InvokeResponse` shapes as standard protobuf-JSON (per the canonical [proto3 JSON mapping](https://protobuf.dev/programming-guides/proto3/#json) — field names become `lowerCamelCase`, `bytes` fields become base64 strings, etc. — this is a well-defined, tool-generated mapping, not a hand-maintained parallel schema).

A plugin author or external caller who never wants to touch protobuf codegen can, e.g., invoke a capability directly via:

```
POST /v1/invoke
Content-Type: application/json

{
  "capability": "forge.data.query",
  "payload": "<base64-encoded capability-specific bytes>",
  "metadata": { "traceId": "abc-123" }
}
```

and get back the equivalent JSON-mapped `InvokeResponse`. This is what makes Document 1 §6 success criterion 2 ("write a plugin in Python with zero Rust knowledge") achievable even by someone who skips gRPC entirely and just uses an HTTP client library plus whatever JSON encoding their capability's payload actually needs at the *content* level (the payload bytes themselves can be anything the plugin defines — many simple plugins just put a plain JSON object's UTF-8 bytes directly in `payload` rather than a second layer of protobuf, and that's a fully supported, common pattern, documented with a worked example in Document 7).

---

## 8. Protocol Versioning Worked Example (making TRD §7 concrete)

- Kernel ships understanding protocol `1.x`.
- A plugin built against protocol `1.0` declares `plugin_protocol_version: "1.0"` at handshake. **Works.**
- Six months later, protocol gains a new optional `metadata` use-convention and bumps to `1.1` (purely additive — no message had a field removed or a required field added). An old `1.0` plugin still works against the new kernel unmodified — it simply never populates the new convention. **Works**, demonstrating the MINOR-bump rule from TRD §7.
- A hypothetical future `2.0` that, say, restructures `InvokeRequest` incompatibly would require every plugin to be rebuilt against the new `.proto`. The kernel, on seeing a `1.x` plugin try to register against a `2.x`-only kernel, **refuses the handshake with an explicit version-mismatch error** rather than attempting best-effort interop — per TRD §7's "never guess" rule.

---

## 9. What This Document Deliberately Leaves to Document 7

This spec defines the wire contract. It does *not* show "how to set up a Python virtualenv and write your first plugin" — that's a step-by-step concern, not a contract concern, and lives in Document 7 so this document stays a stable reference even as tooling/ergonomics guidance evolves around it.
