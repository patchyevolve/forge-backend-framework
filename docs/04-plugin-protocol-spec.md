# Forge Plugin Protocol Specification

## 1. Purpose

This document defines the wire contract between the Forge kernel and any plugin. A plugin author who has read this document and the Plugin Developer's Guide should be able to write a working Forge plugin in any language that supports gRPC.

## 2. Transport Model

All Forge plugins use **managed-subprocess** transport: the Forge kernel spawns the plugin binary as a child process and the plugin binds a gRPC server on a port assigned by the kernel.

```
Forge Kernel                         Plugin Process
     │                                      │
     │  1. Spawns process                    │
     │     with FORGE_LISTEN_ADDR     ───────┤
     │     and FORGE_PLUGIN_NAME             │
     │                                      │
     │  2. Plugin starts gRPC server         │
     │     on FORGE_LISTEN_ADDR              │
     │                                      │
     │  3. ─── gRPC Connect ─────────────→  │
     │                                      │
     │  4. ←── Register (capabilities) ──── │
     │                                      │
     │  5. ←── HealthCheck (periodic) ───── │
     │                                      │
     │  6. ── Invoke (handle request) ────→ │
     │  7. ←── Response ─────────────────── │
     │                                      │
     │  8. ── Drain (shutdown signal) ────→ │
     │                                      │
```

The kernel sets these environment variables for every plugin:

| Variable | Value | Purpose |
|---|---|---|
| `FORGE_LISTEN_ADDR` | `127.0.0.1:<random_port>` | gRPC server bind address |
| `FORGE_CALLBACK_ADDR` | Same as LISTEN_ADDR | Legacy compat |
| `FORGE_PLUGIN_NAME` | Plugin name from forge.toml | Identity |

The kernel assigns a random free port to each plugin, spawns the plugin binary, waits for it to bind, and connects via gRPC.

## 3. Plugin Registration

### 3.1 Configuration in forge.toml

Plugins are defined inline in `forge.toml`:

```toml
[[plugins]]
name = "auth"
path = "target/release/my-project-auth"
capabilities = ["app.auth.login@1.0", "app.auth.verify@1.0"]
```

| Field | Description |
|---|---|
| `name` | Unique plugin name (used in logs, status, restart) |
| `path` | Path to the compiled binary (relative to project root) |
| `capabilities` | List of `name@version` this plugin provides |
| `args` | Optional command-line arguments |
| `env` | Optional extra environment variables |
| `restart_policy` | `on-failure` (default), `always`, `never` |

The kernel reads this at startup, spawns each plugin, and connects.

### 3.2 Actual Capability Registration

The `capabilities` list in `forge.toml` is **advisory metadata** for `forge status` and `forge status --graph`. The **authoritative** capability list is whatever the plugin declares during the live gRPC `Register` handshake (§5). The kernel logs a warning if the two lists differ but never rejects a plugin over it.

## 4. The Handshake

Upon gRPC connection, the kernel calls the plugin's `Register` RPC exactly once:

```
Kernel → Plugin:  RegisterRequest {
                     kernel_protocol_version: "1.0",
                     instance_id: "<uuid>",
                   }

Plugin → Kernel:  RegisterResponse {
                     plugin_protocol_version: "1.0",
                     capabilities: [
                       Capability {
                         name: "app.auth.login",
                         version: "1.0.0",
                         input_schema_ref: "",
                         output_schema_ref: "",
                       },
                     ],
                   }
```

- The kernel checks `plugin_protocol_version` — same MAJOR version required. On mismatch, the plugin is rejected with a clear log message and transitioned to `STOPPED`.
- On success, the kernel registers each capability in its registry and transitions the plugin to `READY`.
- `input_schema_ref`/`output_schema_ref` are informational strings, not kernel-enforced types.

## 5. The ForgePlugin Service Definition

Every plugin implements this gRPC service:

```protobuf
syntax = "proto3";
package forge.plugin.v1;

service ForgePlugin {
  rpc Register(RegisterRequest) returns (RegisterResponse);
  rpc Invoke(InvokeRequest) returns (InvokeResponse);
  rpc HealthCheck(HealthCheckRequest) returns (HealthCheckResponse);
  rpc Drain(DrainRequest) returns (DrainResponse);
}

message RegisterRequest {
  string kernel_protocol_version = 1;
  string instance_id = 2;
}

message RegisterResponse {
  string plugin_protocol_version = 1;
  repeated Capability capabilities = 2;
}

message Capability {
  string name = 1;
  string version = 2;
  string input_schema_ref = 3;
  string output_schema_ref = 4;
}

message InvokeRequest {
  string request_id = 1;
  string capability = 2;
  bytes payload = 3;
  map<string, string> metadata = 4;
}

message InvokeResponse {
  string request_id = 1;
  oneof result {
    bytes payload = 2;
    PluginError error = 3;
  }
}

message PluginError {
  string code = 1;
  string message = 2;
  map<string, string> details = 3;
}

message HealthCheckRequest {}

message HealthCheckResponse {
  bool healthy = 1;
  string detail = 2;
}

message DrainRequest {
  uint32 grace_period_ms = 1;
}

message DrainResponse {}
```

The Rust SDK (`forge::sdk`) wraps this into a `Plugin` trait so users never touch protobuf directly. Other languages compile this `.proto` with their standard `protoc` toolchain.

### RPC Semantics

| RPC | Direction | When | Behavior |
|---|---|---|---|
| `Register` | K→P | Once, immediately after connect | Plugin declares capabilities |
| `Invoke` | K→P | For every incoming request | Plugin handles the capability, returns payload or error |
| `HealthCheck` | K→P | Every 5 seconds (configurable) | Plugin returns `healthy: true/false` |
| `Drain` | K→P | Once during graceful shutdown | Plugin cleans up; kernel waits `grace_period_ms` then force-kills |

## 6. Health Checking

The kernel calls `HealthCheck` on every `READY` plugin every `health_check_interval_ms` (default 5000ms). A non-response within 2000ms counts as a failed check identically to an explicit `healthy: false`.

After `health_check_failure_threshold` (default 3) consecutive failures, the plugin is transitioned to `STOPPED` and the restart policy kicks in. A single subsequent success immediately restores the plugin (no debounce).

## 7. HTTP/JSON Transcoding

The HTTP gateway exposes the same `InvokeRequest`/`InvokeResponse` shapes as JSON. This lets any HTTP client call plugin capabilities without gRPC tools:

```bash
curl -X POST http://localhost:9091/v1/invoke \
  -H "Content-Type: application/json" \
  -d '{
    "capability": "app.auth.login",
    "payload": "eyJ1c2VybmFtZSI6ImFkbWluIiwicGFzc3dvcmQiOiJwYXNzd29yZCJ9"
  }'
```

The payload field is base64-encoded bytes (per the proto3 JSON mapping). `bytes` fields become base64 strings, field names become `lowerCamelCase`.

A simpler path for custom routes configured in `forge.toml`:

```bash
curl -X POST http://localhost:9091/calc/add \
  -H "Content-Type: application/json" \
  -d '{"a":10,"b":3}'
```

In this case the HTTP gateway automatically forwards the JSON body as-is to the plugin's payload bytes, and the plugin's response bytes are wrapped in `{"payload": <response>}`.

## 8. Protocol Versioning

- Kernel ships understanding protocol `1.x`.
- A plugin built against protocol `1.0` declares `plugin_protocol_version: "1.0"` at handshake. **Works.**
- A future protocol `1.1` with additive changes (new optional fields) would still work with `1.0` plugins — they simply never populate the new fields.
- A hypothetical `2.0` with breaking changes would require all plugins to be rebuilt. The kernel refuses the handshake with an explicit version-mismatch error.

## 9. SDK Support

| Language | Package | Approach |
|---|---|---|
| Rust | `forge::sdk` | Implements `Plugin` trait, wraps `PluginServer` |
| Python | Manual | Compile `.proto`, implement the gRPC service |
| Any gRPC language | Manual | Compile `.proto`, implement 4 RPCs |
