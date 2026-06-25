# Forge — Operator's Guide

**Document 6 of 7 — Step-by-Step**
**Status:** Final
**Depends on:** All prior documents (this is where they become actions)
**Audience:** Anyone running Forge — including people who never need to read Rust

---

## 0. Before You Start

You need: a Linux machine (Fedora or Ubuntu; this guide uses Fedora command equivalents where they differ), `curl`, and — only if building from source instead of using a prebuilt binary — `rustup`. Nothing else.

If you just want to see Forge do something in five minutes, skip to §6 (Quickstart Demo) and come back to §1–5 afterward.

---

## 1. Install

### 1.1 Prebuilt binary (recommended)

```bash
curl -fsSL https://example.invalid/install.sh | sh
```

This is the script described in Build & Distribution Spec §3.1. It downloads a static binary matching your OS/architecture, verifies its checksum, and places it at `~/.local/bin/forge`. Confirm:

```bash
forge --version
# forge 1.0.0 (protocol 1.0, manifest-schema 1.0)
```

If `forge: command not found`, your shell's `PATH` doesn't include `~/.local/bin`. Add this to your `~/.bashrc` (or `~/.zshrc`):

```bash
export PATH="$HOME/.local/bin:$PATH"
```

then `source ~/.bashrc` (or open a new terminal).

### 1.2 Build from source

```bash
git clone https://example.invalid/forge.git
cd forge
cargo build --release -p forge-cli
sudo cp target/release/forge /usr/local/bin/forge
```

Use this path if you want to inspect/audit the source before running it (relevant if you're using Forge for the security-tool showcase) or if you're on a target triple without a prebuilt artifact.

### 1.3 Minimal/embedded profile

When gRPC gateway and plugin lifecycle management aren't needed — e.g. embedding Forge's dispatch core inside an existing application, or targeting a constrained environment — strip the tonic dependency:

```bash
cargo build -p forge-core --no-default-features
```

This disables:
- **tonic** (gRPC client/server) — Bus dispatch falls back to in-process handlers only.
- **Manager** (plugin lifecycle) — `start_all`, `restart_plugin`, `shutdown_all` are unavailable.
- **tokio `full` features** — only `rt`, `macros`, `sync`, `time` are load-bearing.

What remains: `Registry`, `Bus` (in-process dispatch via `register_handler`), config loader, lifecycle `PluginState` type, and the embedding `Kernel` API. See `examples/embedded-minimal/` for a complete 19-line example that uses only this profile. In `Cargo.toml`:

```toml
forge-core = { version = "1.0", default-features = false }
```

To add tonic back for individual features, enable the `tonic` feature explicitly:

```toml
forge-core = { version = "1.0", features = ["tonic"] }
```

The default enables `tonic` — use `default-features = false` only when you've confirmed you don't need gRPC or plugin management.

---

## <a id="embedding"></a>2. Embedding Forge in an Application

Instead of running Forge as a standalone binary with plugins, you can embed its dispatch core directly into your Rust application. This is useful when you want in-process capability routing without the overhead of gRPC listeners, plugin subprocess management, or TLS.

Add to your `Cargo.toml`:

```toml
[dependencies]
forge-core = { version = "1.0", default-features = false }
tokio = { version = "1", features = ["rt", "macros"] }
bytes = "1"
```

Then register handlers and dispatch:

```rust
use forge_core::bus::{Bus, Invocation, InvocationError};
use forge_core::registry::Registry;
use forge_core::kernel::{Kernel, KernelConfig};

let kernel = Kernel::new(KernelConfig::default()).await;
kernel.bus().register_handler("ping", |inv| async move {
    Ok(bytes::Bytes::from("pong"))
}).await;
let result = kernel.bus().dispatch(Invocation::simple("ping", "hello")).await;
assert_eq!(result.unwrap(), &b"pong"[..]);
```

A complete working example is at `examples/embedded-minimal/` (19 lines). The embedding API uses only in-process dispatch (`register_handler` + `dispatch`) and does not start any gRPC listeners, health checks, or plugin subprocesses. Signal handling, config file parsing, and gateway TLS are the caller's responsibility.

---

## 3. Your First `forge.toml`

Forge needs exactly one top-level config file (Architecture Spec §2.4) describing the kernel itself — not your plugins, just the kernel. Create one:

```bash
mkdir -p ~/forge-demo && cd ~/forge-demo
cat > forge.toml << 'EOF'
forge_config_version = "1.0"

[gateway]
grpc_bind = "127.0.0.1:9090"
http_bind = "127.0.0.1:9091"
tls = false   # fine for local dev; see §7 before exposing this beyond localhost

[log]
level = "info"

[plugins]
manifest_dir = "./plugins"   # forge scans this directory for plugin.forge.toml files
EOF

mkdir -p plugins
```

Every field here maps directly to a `config` responsibility from Architecture Spec §2.4. You don't need to understand the kernel internals to set these — just know that `manifest_dir` is where you'll drop plugin folders next.

---

## 4. Adding Your First Plugin

You need at least one plugin manifest before `forge run` has anything to serve. This guide uses the official SQLite data plugin (`forge-plugin-data-sqlite`, from the `plugins-official` collection, Build & Distribution Spec §2) as the example — full instructions for *writing your own* plugin are in Document 7; this section is purely about *installing* one someone else (including official Forge) already wrote.

```bash
mkdir -p plugins/data-sqlite
cat > plugins/data-sqlite/plugin.forge.toml << 'EOF'
forge_manifest_version = "1.0"

[plugin]
name = "data-sqlite"
version = "0.3.1"
description = "SQLite-backed persistence (official reference plugin)"
protocol_version = "1.0"

[transport]
shape = "managed-subprocess"
executable = "/usr/local/bin/forge-plugin-data-sqlite"
args = []

[lifecycle]
restart_policy = "on-failure"
restart_backoff_initial_ms = 500
restart_backoff_max_ms = 30000
restart_max_attempts = 5
health_check_interval_ms = 5000
health_check_failure_threshold = 3
drain_grace_period_ms = 10000

[capabilities]
provides = ["forge.data.query@1.0", "forge.data.write@1.0"]
requires = []

[env]
DATABASE_PATH = "./data/demo.db"
EOF

mkdir -p plugins/data-sqlite/data
```

This is a literal instance of the manifest template from Plugin Protocol Spec §3 — every field there maps one-to-one to a field here.

---

## 5. Running Forge

```bash
forge run --config ./forge.toml
```

What you should see (paraphrased — exact log formatting may differ slightly by version):

```
[INFO] forge-cli 1.0.0 starting
[INFO] config loaded from ./forge.toml (forge_config_version 1.0 — OK)
[INFO] gateway: gRPC listening on 127.0.0.1:9090
[INFO] gateway: HTTP listening on 127.0.0.1:9091 (TLS disabled — local dev only)
[INFO] plugin discovered: data-sqlite (./plugins/data-sqlite/plugin.forge.toml)
[INFO] plugin data-sqlite: CONNECTING (managed-subprocess, spawning /usr/local/bin/forge-plugin-data-sqlite)
[INFO] plugin data-sqlite: HANDSHAKING
[INFO] plugin data-sqlite: READY — capabilities registered: forge.data.query@1.0.0, forge.data.write@1.0.0
[INFO] forge-cli ready — 1 plugin connected, 2 capabilities live
```

This sequence is the literal `DISCOVERED → CONNECTING → HANDSHAKING → READY` path from Architecture Spec §2.1, now visible as log lines instead of an abstract diagram. If a plugin gets stuck or fails, the log line tells you exactly which state it stalled in — that's the entire point of the state machine being explicit rather than a vague "connected/not connected" flag.

Leave this running in its terminal; open a new terminal for the rest of this guide.

---

## 6. Quickstart Demo (Five-Minute Version)

If you only want to *see* something work right now, before setting up the full example backend:

```bash
curl -s http://127.0.0.1:9091/healthz
# {"status":"ok"}

curl -s http://127.0.0.1:9091/v1/status
# shows the live capability registry — this is registry::list_capabilities()
# from Architecture Spec §2.2, exposed over HTTP
```

This already demonstrates the gateway → registry path end-to-end with zero plugin-specific knowledge required.

---

## 7. Operating Forge

### 7.1 Checking status

```bash
forge status
```

Shows every discovered plugin, its current lifecycle state (§2.1's state machine, literally), and every live capability. This is your first diagnostic step for *any* problem — "what does `forge status` say" before anything else.

```bash
forge status --graph
```

Renders the `provides`/`requires` advisory graph from every manifest (Plugin Protocol Spec §3) — useful for spotting a missing dependency before you even start the kernel.

### 7.2 Restarting a stuck plugin

```bash
forge plugin restart data-sqlite
```

Forces a `DRAINING → STOPPED → DISCOVERED → ... → READY` cycle (Architecture Spec §6) for that one plugin — exactly the same path a hot-reload takes, just operator-triggered instead of file-watch-triggered.

### 7.3 Hot-reloading after a manifest edit

If `[plugins] watch = true` is set in `forge.toml`, editing a `plugin.forge.toml` file (e.g. bumping a restart policy) triggers the reload cycle automatically — no kernel restart needed. Watch the logs; you'll see the same `DRAINING`/`STOPPED`/`DISCOVERED` sequence as a manual restart.

### 7.4 Stopping Forge

```
Ctrl+C in the terminal running `forge run`
```

or, if running as a systemd service (see §8):

```bash
sudo systemctl stop forge
```

Either way, every connected plugin receives a `Drain` RPC (Plugin Protocol Spec §5) before the kernel exits, respecting each plugin's `drain_grace_period_ms`.

---

## 8. Security Hardening (Read This Before Exposing Forge Beyond `127.0.0.1`)

Per TRD §8, plaintext is the default for local development and Forge will say so loudly on every startup. Before binding to anything other than `127.0.0.1` or a private network:

1. **Enable TLS.** In `forge.toml`:
   ```toml
   [gateway]
   grpc_bind = "0.0.0.0:9090"
   http_bind = "0.0.0.0:9091"
   tls = true
   tls_cert_path = "/etc/forge/tls/cert.pem"
   tls_key_path = "/etc/forge/tls/key.pem"
   ```
2. **Lock down Unix socket permissions.** If any plugin uses `transport.shape = "server"` with a Unix socket address, confirm the socket file is created `0600` (TRD §8's default) — verify with `ls -l /run/forge/plugins/`.
3. **Remember plugins are trusted code** (TRD §8). Forge v-final does not sandbox plugins. Only point `manifest_dir` at plugins you've reviewed or built yourself. This is the single most important sentence in this entire guide for the security-showcase use case — say it out loud if you're demoing this to someone: *Forge's threat model assumes plugins are trusted; it is an orchestration boundary, not a sandbox.*
4. **No required outbound network calls.** Confirm this yourself for your own peace of mind: `forge run` with no internet connectivity at all, after the binary is already on disk, starts and serves traffic identically (PRD §6 success criterion 7). If you ever see Forge itself making an unexpected outbound call, that's a bug — file it against whichever plugin actually made the call first, since the kernel has no code path that does this.

---

## 9. Running as a systemd Service

```ini
# /etc/systemd/system/forge.service
[Unit]
Description=Forge backend kernel
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/forge run --config /etc/forge/forge.toml
Restart=on-failure
RestartSec=5
User=forge
Group=forge

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd --system --no-create-home forge
sudo mkdir -p /etc/forge && sudo cp forge.toml /etc/forge/
sudo systemctl daemon-reload
sudo systemctl enable --now forge
sudo systemctl status forge
journalctl -u forge -f    # live logs
```

Note this is **kernel** process supervision (systemd restarting the whole `forge` process if it dies) layered on top of, not a replacement for, the **plugin** lifecycle/restart policy (Architecture Spec §5) that Forge itself handles internally for individual plugins. The two operate at different levels and are both useful simultaneously.

---

## 10. Upgrading

1. Check the running version: `forge --version`.
2. Check the target version's protocol/manifest-schema versions against your current plugins' declared `protocol_version` (Plugin Protocol Spec §3) — per TRD §7, a MAJOR protocol bump means your plugins may need rebuilding; a MINOR/PATCH bump is safe to assume compatible.
3. Replace the binary (re-run the install script, or rebuild from source) — this never touches your `forge.toml` or plugin manifests, which live entirely outside the binary.
4. Restart: `sudo systemctl restart forge` (or `Ctrl+C` + re-run, for the non-systemd path).
5. Confirm with `forge status` — every plugin should walk back through to `READY` exactly as it did on first start, since upgrade-restart and first-start use the identical lifecycle path (Architecture Spec §2.1 — there is no separate "upgrade mode").

---

## 11. Observability

With the `metrics` build feature enabled (Build & Distribution Spec §5):

```bash
curl -s http://127.0.0.1:9091/metrics
```

Exposes Prometheus-format counters/histograms for: invocations per capability, invocation latency (validating the TRD §6 budget — watch the `forge_invocation_duration_seconds` histogram's p99 against the documented <1ms-for-loopback target), plugin lifecycle transition counts, and health-check failure counts. Point any standard Prometheus/Grafana setup at this endpoint; nothing Forge-specific is required on the observability-tooling side.

---

## 12. Troubleshooting Quick Reference

| Symptom | Likely cause | Where to look |
|---|---|---|
| Plugin stuck in `CONNECTING` | executable path wrong, or socket address unreachable | `forge status`, check `transport` section of that plugin's manifest |
| Plugin stuck in `HANDSHAKING` | protocol version mismatch | kernel log will show the explicit version-mismatch error (Plugin Protocol Spec §8) |
| Plugin flaps `READY ↔ DEGRADED` | health check failing intermittently — slow plugin, or `health_check_failure_threshold` too aggressive for this plugin's normal latency | raise `health_check_interval_ms`/threshold in that plugin's manifest |
| `forge run` exits immediately with a config error | `forge_config_version` or a `plugin.forge.toml`'s `forge_manifest_version` major version unrecognized by this kernel build | the startup error names the exact file and version mismatch — never a silent failure, per TRD §7 |
| Request hangs past expected response time | check the capability's deadline configuration; bus enforces `deadline` per Architecture Spec §5, so a hang past that should resolve to `DeadlineExceeded`, not hang forever — if it genuinely hangs forever, that's a bug worth filing | `forge status` to see if the target plugin is `DEGRADED` |

---

## 13. Forward Reference

If the plugin you need doesn't exist yet, Document 7 is the rest of this story — it takes you from "I have Forge running" to "I wrote the plugin myself."
