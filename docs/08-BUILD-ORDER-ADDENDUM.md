# Forge — Build Order Addendum (Steps 8–14)

**Append this after step 7 in `00-MASTER-AGENT-PROMPT.md` §2 ("Build Order"). Steps 1–7 are done and verified — do not reopen them without a specific reason. This addendum exists because the last session incorrectly invented a "step 8" that didn't exist; these are the real next steps, in order, mapped directly to PRD §6's 7 success criteria so there's no ambiguity about what "done" means for any of them.**

Same rules as the rest of the master prompt apply: §5 ("ask, don't guess") and §6 (anti-drift) are still in force. Don't reorder these without telling the user why. Don't mark any of them done on the strength of a status report — the user (or their reviewer) will read the actual files and re-run the tests before agreeing.

---

8. **Failure-semantics proof** (closes PRD §6 criterion 5). Architecture Spec §5 ("failure semantics") already specifies the expected behavior — re-read it before touching code. Against the assembled `examples/example-backend/`:
   - Start all four plugins + kernel via `test_committed_backend.sh`'s pattern (committed manifests, no overrides).
   - Mid-request, `kill -9` the `data-sqlite` process while an in-flight `forge.data.query` invocation is pending.
   - Confirm the caller receives `InvocationError::TransportError` (or the HTTP/gRPC equivalent) — not a hang, not a panic in the kernel.
   - Confirm the kernel process and the other three plugins are still alive and responsive immediately after.
   - Confirm the registry deregisters `forge.data.query`/`forge.data.write` so a *second* call right after fails fast with `NotFound`, not another hang — this is the detail most likely to be skipped, check it explicitly.
   - Confirm `restart_policy = "on-failure"` actually respawns the plugin (per `PluginLifecycleConfig`'s backoff fields in `forge/src/config.rs`) and that it re-registers once healthy.
   - Write this as a script (`test_kill_resilience.sh` or similar) that automates the kill and asserts on each point above — not a manual one-off run. Show the script and its output.

9. **Request-ID generation and propagation** (small, well-scoped, do alongside step 8). Right now `request_id` is never populated at any ingress point:
   - `forge/src/gateway/grpc.rs:116,139` — `request_id: String::new()`
   - `forge/src/gateway/http.rs:198,234,252` — `request_id: None`
   - `forge/src/bus.rs:108` — `request_id: String::new()`
   `uuid` is already a workspace dependency and already used for `instance_id` generation in `lifecycle/manager.rs` — follow that exact pattern. Generate a UUID at each gateway ingress point (gRPC and HTTP), thread it through `Invocation` → `InvokeRequest` → back through `InvokeResponse`, and log it at each hop (gateway, bus dispatch, plugin invoke) so a single logical request is traceable across a multi-hop chain like router→auth→data. Add a test that issues one `forge.http.route` call through the full chain and asserts the same request ID (or a clearly-correlated child ID, your choice — state which) appears in the logs of all three plugins involved.

10. **Embedding example** (closes PRD §6 criterion 3). Operator's Guide §[embedding] describes this — re-read it. Write a new example crate (`examples/embedded-minimal/` or similar) that depends only on `forge` (without the `gateway` feature), registers a single in-process invocation path, and runs it. Count the lines yourself in the final file and state the count in your response — the criterion is "under 20 lines," not "approximately short."

11. **Distribution path** (closes PRD §6 criterion 1). Per Build & Distribution Spec §3, Shape 1 (installable executable): write the installer script and confirm a prebuilt-binary path per target triple is at least scaffolded (cross-compilation doesn't need to be exhaustive — document which triples are actually tested vs. just declared). Also produce the crates.io-publishable-shape crate per Shape 2 of that same section, even though actual publishing is out of scope. Time this *after* steps 8–10 — don't package a binary that hasn't passed the failure-semantics check yet.

12. **Offline build/run proof** (closes PRD §6 criterion 7). After an initial `cargo fetch`, cut network access entirely (use the same kind of egress allowlist/sandbox pattern the reviewer has been using) and confirm `cargo build` and `forge run --config examples/example-backend/forge.toml` both succeed with zero network calls. If anything fails, the most likely culprit is a crate that isn't using a `bundled`/vendored feature flag the way `rusqlite` already does — find and fix it the same way, don't just exempt the offline requirement.

13. **Minimal/embedded build profile** (closes PRD §6 criterion 7's documentation half, and PRD §5.1's last bullet). This one is documentation-weighted, not implementation-weighted — PRD explicitly says full ESP32 wiring is a stretch goal, not a requirement. Add a feature-gated minimal profile to `forge`'s `Cargo.toml` (no `gateway` feature, no `tokio` full-features, whatever's actually load-bearing for a constrained target) and document it in the Operator's Guide. Timebox this — don't let it expand into real embedded hardware work.

14. **Doc-vs-code audit** (closes PRD §6 criterion 6 — **do this last, immediately before any new comprehensive usage guide is written, not after**). Cross-reference Documents 3 (Architecture Spec), 4 (Plugin Protocol Spec), 6 (Operator's Guide), and 7 (Plugin Developer's Guide) against the actual current source, field by field and lifecycle-state by lifecycle-state. Specifically check:
    - Every field in `PluginManifest`/`PluginLifecycleConfig`/`PluginCapabilitiesDecl` (`forge/src/config.rs`) is documented in Document 4, with matching defaults.
    - Every `PluginState` transition the lifecycle manager actually implements matches Document 3 §2.1 exactly, including illegal-transition rejection.
    - The `ResolutionStrategy::RoundRobin` and "first-ready wins" default (Document 3 §2.2) are both accurately described where they're documented.
    - Every CLI subcommand in `cli/src/main.rs` matches what Document 6 §6 shows.
    Report findings as a literal gap list (doc section vs. file:line), not a prose summary — the user will spot-check a sample of the list against source before accepting "zero gaps."

---

**After step 14 passes, and only then:** the full-depth usage guide is in scope. At that point it should largely be "make Documents 6 and 7 comprehensive and correct against an already-audited codebase," not a from-scratch write — if it feels like you're discovering new undocumented behavior while writing the guide, stop and go back to step 14, because that means the audit wasn't actually complete.
