# Forge — Plugin Developer's Guide

**Document 7 of 7 — Step-by-Step**
**Status:** Final
**Depends on:** Document 4 (Plugin Protocol Specification) most directly; references Document 3 for *why* things work this way
**Audience:** Anyone writing a plugin, in any language — this guide proves the "any language" claim with two worked examples (Rust and Python) plus patterns for everything else

---

## 0. The One Idea to Hold Onto

A Forge plugin is: a program that (1) implements four gRPC methods (`Register`, `Invoke`, `HealthCheck`, `Drain` — Plugin Protocol Spec §5), and (2) ships a manifest file telling Forge how to find it. That's the entire interface. Everything below is detail on top of that one sentence.

---

## 1. Decide: Shape A or Shape B?

From Plugin Protocol Spec §2 — pick based on whether you want to manage your own process lifecycle:

- **Shape A (plugin-as-server)**: your plugin listens; Forge dials in. Pick this if your plugin is a long-running service you'd run/restart yourself anyway.
- **Shape B (managed-subprocess)**: Forge spawns and restarts your plugin for you, your plugin dials *out* to a callback address Forge gives it via `FORGE_CALLBACK_ADDR`. Pick this for simple scripts where you'd rather not write process-supervision logic.

The Python example below uses Shape B (simplest for a script). The Rust example uses Shape A (idiomatic for a standalone Rust service). Both are fully valid for any language — the choice is about supervision convenience, not language capability.

---

## 2. Worked Example: A Rust Plugin (Shape A)

We'll build a minimal capability, `forge.example.echo`, that just echoes its input back uppercased — small enough to see the whole protocol with nothing else competing for attention.

### 2.1 Project setup

```bash
cargo new forge-plugin-echo-rs
cd forge-plugin-echo-rs
```

```toml
# Cargo.toml
[dependencies]
forge-plugin-sdk = "1.0"   # the optional ergonomic wrapper, Build & Distribution Spec §2 —
                            # generated stubs directly from forge-proto would work too, this
                            # just saves boilerplate
tokio = { version = "1", features = ["full"] }
```

### 2.2 The plugin code

```rust
// src/main.rs
use forge_plugin_sdk::{PluginServer, Capability, InvokeContext, InvokeResult, PluginError};

struct EchoPlugin;

#[forge_plugin_sdk::async_trait]
impl forge_plugin_sdk::Plugin for EchoPlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::new("forge.example.echo", "1.0.0")]
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.example.echo" => {
                // payload is raw bytes (Plugin Protocol Spec §5) — this example
                // treats them as UTF-8 text directly, the common simple-plugin
                // pattern noted in Plugin Protocol Spec §7.
                let text = String::from_utf8_lossy(&ctx.payload);
                Ok(text.to_uppercase().into_bytes())
            }
            other => Err(PluginError::not_found(format!("unknown capability: {other}"))),
        }
    }

    async fn health_check(&self) -> bool {
        true // this plugin has no external dependencies to check
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // listens on the Unix socket address from this plugin's own manifest —
    // the SDK reads FORGE_LISTEN_ADDR, which Forge's lifecycle module sets
    // when spawning/connecting, matching transport.address in the manifest.
    PluginServer::new(EchoPlugin)
        .serve_shape_a()
        .await
}
```

This is the entire plugin. `forge_plugin_sdk::Plugin` is a thin trait wrapping the four-RPC contract from Plugin Protocol Spec §5 — under the hood, `serve_shape_a()` is doing exactly the `Register`/`Invoke`/`HealthCheck`/`Drain` dance, generated from `forge-proto`'s stubs, with `capabilities()`/`invoke()`/`health_check()` plugged into the right RPC handlers. You never see `tonic` directly unless you want to.

### 2.3 The manifest

```toml
# plugin.forge.toml
forge_manifest_version = "1.0"

[plugin]
name = "echo-rs"
version = "0.1.0"
description = "Minimal echo plugin (Rust, Shape A)"
protocol_version = "1.0"

[transport]
shape = "server"
address = "unix:///run/forge/plugins/echo-rs.sock"

[lifecycle]
restart_policy = "on-failure"
restart_backoff_initial_ms = 500
restart_backoff_max_ms = 30000
restart_max_attempts = 5
health_check_interval_ms = 5000
health_check_failure_threshold = 3
drain_grace_period_ms = 5000

[capabilities]
provides = ["forge.example.echo@1.0"]
requires = []
```

### 2.4 Build, place, run

```bash
cargo build --release
mkdir -p ~/forge-demo/plugins/echo-rs
cp target/release/forge-plugin-echo-rs ~/forge-demo/plugins/echo-rs/
cp plugin.forge.toml ~/forge-demo/plugins/echo-rs/
# but note: shape = "server" means YOU run this binary yourself, separately —
# Forge only DIALS it, per Plugin Protocol Spec §2's Shape A definition.
# Start it first:
mkdir -p /run/forge/plugins   # or wherever you pointed `address` at
./target/release/forge-plugin-echo-rs &
# THEN start/reload forge — it'll find the manifest in manifest_dir and
# connect to the already-listening socket.
```

### 2.5 Test it

```bash
curl -s -X POST http://127.0.0.1:9091/v1/invoke \
  -H "Content-Type: application/json" \
  -d '{"capability": "forge.example.echo", "payload": "aGVsbG8="}'
# payload "aGVsbG8=" is base64 for "hello" (HTTP/JSON transcoding, Plugin
# Protocol Spec §7, base64-encodes raw bytes fields)
# expect: {"requestId": "...", "payload": "SEVMTE8="}  ("HELLO" in base64)
```

You've now exercised the full path from Architecture Spec §3's request walkthrough, end to end, with a plugin you wrote yourself.

---

## 3. Worked Example: A Python Plugin (Shape B) — Zero Rust Required

This is the literal scenario PRD §6 success criterion 2 demands. Same `forge.example.echo` capability, different language, different shape, to show both axes of variation.

### 3.1 Setup

```bash
mkdir forge-plugin-echo-py && cd forge-plugin-echo-py
python3 -m venv venv && source venv/bin/activate
pip install grpcio grpcio-tools
```

### 3.2 Generate stubs from the canonical proto

Take `forge_plugin_v1.proto` directly from the Forge repository (Plugin Protocol Spec §5 — this is literally the same file the Rust SDK is built from):

```bash
python -m grpc_tools.protoc \
  -I. --python_out=. --grpc_python_out=. \
  forge_plugin_v1.proto
# produces forge_plugin_v1_pb2.py and forge_plugin_v1_pb2_grpc.py
```

### 3.3 The plugin code

```python
# plugin.py
import os
import grpc
import asyncio
import forge_plugin_v1_pb2 as pb
import forge_plugin_v1_pb2_grpc as pb_grpc

class EchoPlugin(pb_grpc.ForgePluginServicer):
    async def Register(self, request, context):
        return pb.RegisterResponse(
            plugin_protocol_version="1.0",
            capabilities=[
                pb.Capability(
                    name="forge.example.echo",
                    version="1.0.0",
                    input_schema_ref="raw text",
                    output_schema_ref="raw text",
                )
            ],
        )

    async def Invoke(self, request, context):
        if request.capability == "forge.example.echo":
            text = request.payload.decode("utf-8")
            return pb.InvokeResponse(
                request_id=request.request_id,
                payload=text.upper().encode("utf-8"),
            )
        return pb.InvokeResponse(
            request_id=request.request_id,
            error=pb.PluginError(code="NOT_FOUND", message=f"unknown capability: {request.capability}"),
        )

    async def HealthCheck(self, request, context):
        return pb.HealthCheckResponse(healthy=True, detail="ok")

    async def Drain(self, request, context):
        return pb.DrainResponse()

async def main():
    # Shape B: dial OUT to Forge's callback address, per Plugin Protocol
    # Spec §2 — Forge sets this env var when spawning us as a managed subprocess.
    callback_addr = os.environ["FORGE_CALLBACK_ADDR"]
    server = grpc.aio.server()
    pb_grpc.add_ForgePluginServicer_to_server(EchoPlugin(), server)
    server.add_insecure_port(callback_addr)
    await server.start()
    await server.wait_for_termination()

if __name__ == "__main__":
    asyncio.run(main())
```

Notice this Python code implements *exactly* the same four RPCs as the Rust example's SDK-wrapped version — `Register`, `Invoke`, `HealthCheck`, `Drain` — just without a convenience SDK hiding the gRPC plumbing. This is the proof, in code rather than in prose, that Document 4's protocol is genuinely language-neutral: nothing here is Rust-shaped.

### 3.4 The manifest

```toml
forge_manifest_version = "1.0"

[plugin]
name = "echo-py"
version = "0.1.0"
description = "Minimal echo plugin (Python, Shape B)"
protocol_version = "1.0"

[transport]
shape = "managed-subprocess"
executable = "/path/to/forge-plugin-echo-py/venv/bin/python3"
args = ["/path/to/forge-plugin-echo-py/plugin.py"]
working_dir = "/path/to/forge-plugin-echo-py"

[lifecycle]
restart_policy = "on-failure"
restart_backoff_initial_ms = 500
restart_backoff_max_ms = 30000
restart_max_attempts = 5
health_check_interval_ms = 5000
health_check_failure_threshold = 3
drain_grace_period_ms = 5000

[capabilities]
provides = ["forge.example.echo@1.0"]
requires = []
```

Note this `forge.example.echo` capability name is identical to the Rust plugin's. **Don't run both manifests in the same Forge instance pointed at the same capability simultaneously** unless you specifically want to exercise the multi-provider resolution behavior from Architecture Spec §2.2 ("first-ready wins" by default) — for a clean test, use only one at a time, or rename one to `forge.example.echo.py` to run both side by side.

### 3.5 Place and run

```bash
mkdir -p ~/forge-demo/plugins/echo-py
cp plugin.py ~/forge-demo/plugins/echo-py/
cp plugin.forge.toml ~/forge-demo/plugins/echo-py/
# Shape B means Forge spawns this for you — just (re)start `forge run`,
# no separate manual launch step needed, unlike the Shape A example.
```

Test exactly the same way as §2.5 — the HTTP caller has no idea, and no need to know, which language answered.

---

## 4. Patterns for Other Languages

The same recipe generalizes directly — generate gRPC stubs from `forge_plugin_v1.proto` (every mainstream language's `protoc` plugin supports this), implement the four-method service, and either listen (Shape A) or dial the callback address (Shape B):

- **Go**: `protoc-gen-go-grpc`, implement `ForgePluginServer` interface, `grpc.NewServer()`.
- **C/C++**: `grpc_cpp_plugin`, implement the generated service base class.
- **Node/TypeScript**: `@grpc/grpc-js` + `ts-proto` or `grpc-tools`.
- **Anything with only an HTTP client (no gRPC library available)**: skip gRPC entirely and use the HTTP/JSON on-ramp (Plugin Protocol Spec §7) for *outbound* calls into Forge from your plugin's own logic — but note that being *invoked* as a plugin still requires implementing the `Invoke` RPC somehow, so a plugin that wants to receive traffic generally does need *some* gRPC server capability in its language, even a minimal one. Pure HTTP-only-no-gRPC plugins are a documented gap acknowledged here rather than glossed over: if your target language genuinely cannot run a gRPC server, the realistic option today is wrapping your logic in a thin Python or Go shim (or Rust, using the SDK) that does speak gRPC and shells out to / calls into your actual logic. A native non-gRPC inbound transport for plugins is credible future work, not a v-final guarantee.

---

## 5. The Routing-Plugin Pattern (Two Valid Approaches)

Per Architecture Spec §2.3, the kernel dispatches by opaque capability string only. When building something HTTP-router-shaped, you have two valid design choices — pick based on how many routes you have and whether you want Forge's registry to "see" each route individually:

**Approach 1 — Coarse capability, internal sub-dispatch.** Register one capability, `forge.http.route`, and do your own path/method matching *inside* the plugin (e.g. using a Rust router crate like `matchit`, or Python's `re`, internally). Simple, scales to many routes without registry bloat. The kernel only ever sees one capability name.

**Approach 2 — Fine-grained capabilities per route.** Register `forge.http.route.users.get`, `forge.http.route.users.post`, etc. — one capability per route. Lets `forge status` show every route individually and lets different routes even live in *different* plugins/processes. More registry entries, but more operator visibility and more deployment flexibility (you could scale just the hot route to its own plugin instance later).

Neither is "more correct" — Architecture Spec §3's walkthrough uses Approach 2 purely because it's clearer to narrate step by step; production systems often start with Approach 1 for simplicity and split into Approach 2 only for routes that need independent scaling or ownership.

---

## 6. Testing Your Plugin in Isolation (Before Forge Is Even Involved)

Because the protocol is just gRPC, you can test your plugin with any gRPC client, without running Forge at all:

```bash
grpcurl -plaintext -unix /run/forge/plugins/echo-rs.sock \
  forge.plugin.v1.ForgePlugin/HealthCheck
# {"healthy": true}

grpcurl -plaintext -unix /run/forge/plugins/echo-rs.sock \
  -d '{"requestId":"test-1","capability":"forge.example.echo","payload":"aGVsbG8="}' \
  forge.plugin.v1.ForgePlugin/Invoke
```

This decoupling — your plugin is a complete, independently testable gRPC service with no Forge-specific test harness required — is a direct payoff of Document 4 §2's "plugins are just gRPC clients/servers" design choice, and it's worth calling out explicitly in any portfolio writeup: **plugins are unit-testable without the kernel running at all.**

---

## 7. Building the PRD §5.1 Example Backend (Putting It All Together)

The full example backend referenced throughout this suite combines three plugins:

1. `forge-plugin-http-router` (official) — registers fine-grained `forge.http.route.*` capabilities (Approach 2, §5 above).
2. `forge-plugin-auth-jwt` (official) — provides `forge.auth.verify`, called *by* the router plugin per request (the plugin-to-plugin invocation pattern from Architecture Spec §3 step 5).
3. `forge-plugin-data-sqlite` (official) — provides `forge.data.query`/`forge.data.write`, also called by the router plugin.

None of these three plugins know about each other's existence in code — each only knows the *capability names* it needs to call, resolved at runtime through the registry (Architecture Spec §2.2). This is the architecture's central claim made concrete: you can swap `forge-plugin-data-sqlite` for a hypothetical `forge-plugin-data-postgres` by changing one manifest, with zero code changes anywhere else in the system, because the router plugin only ever asked for `forge.data.query`, never for "SQLite" specifically.

Full manifests for all three official plugins ship in `plugins-official/*/plugin.forge.toml` (Build & Distribution Spec §2) as ready-to-copy starting points — this guide has now shown you everything needed to read and modify them yourself.

---

## 8. Checklist Before You Call a Plugin "Done"

- [ ] Manifest declares an accurate `forge_manifest_version` and `protocol_version`.
- [ ] `capabilities.provides` in the manifest matches exactly what `Register`'s `RegisterResponse` actually returns at runtime (Plugin Protocol Spec §3's advisory-but-should-match rule).
- [ ] `HealthCheck` reflects real health (if your plugin depends on a database connection, `healthy: false` when that connection is down — don't hardcode `true`).
- [ ] You've tested the plugin standalone with `grpcurl` (§6) before ever pointing Forge at it.
- [ ] You've decided Shape A vs Shape B deliberately (§1), not by accident.
- [ ] Errors returned via `PluginError` have a meaningful `code`, not just a `message` — callers (including other plugins) may branch on `code`.
