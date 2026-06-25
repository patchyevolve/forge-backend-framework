# Forge — Master Agent Build Prompt

**Paste this entire file as the system/first message of every new session. Do not summarize it, do not skip sections — re-read it fully each time before writing or changing any code.**

---

## 0. What You Are Doing

You are implementing **Forge**, a polyglot backend microkernel, against seven finished specification documents that already exist in this repository at `docs/01-PRD.md` through `docs/07-plugin-developers-guide.md`, plus the canonical `docs/forge_plugin_v1.proto`. **These documents are not drafts you can improve on the fly. They are final.** Your job is implementation, not redesign. If something in the docs seems wrong or incomplete, say so explicitly to the user and stop — do not silently "fix" it by deviating in code, and do not silently fill gaps with your own judgment call without flagging it first.

Before doing ANY work in a session, run, in order:
1. `cat docs/01-PRD.md`
2. `cat docs/02-TRD.md`
3. `cat docs/03-architecture-spec.md`
4. `cat docs/04-plugin-protocol-spec.md`
5. `cat docs/05-build-distribution-spec.md`
6. List what you are about to build/change and which document section it implements, before writing code.

If you cannot read these files (wrong working directory, files missing), STOP and tell the user — do not proceed from memory of a prior session's summary of them.

---

## 1. The Five Sentences You Must Never Violate

If you remember nothing else mid-session, remember these. Every one of them has caused a real design failure mode in projects like this, which is why each is stated as a hard rule, not a suggestion.

1. **The kernel has no idea what HTTP routing, authentication, or persistence are.** It only knows opaque capability strings and bytes. If you write a single `if path == "/users"` or `if header == "Authorization"` anywhere inside `forge-core` or `forge-gateway`, you have broken the architecture — that logic belongs inside a *plugin*, full stop, no exceptions for "just this once" or "just for the demo."
2. **A plugin is a process speaking the gRPC service defined in `forge_plugin_v1.proto` — never a Rust trait object loaded via `dyn`, and never a hardcoded match arm in the kernel.** The ONLY four RPCs that exist are `Register`, `Invoke`, `HealthCheck`, `Drain`. Do not invent a fifth. Do not add a kernel-side special case for "the official plugins" that bypasses this — they connect exactly the same way a third-party plugin would.
3. **Every wire-format change requires a version bump**, per TRD §7: protocol MAJOR.MINOR.PATCH, manifest schema MAJOR.MINOR, kernel binary MAJOR.MINOR.PATCH — tracked separately, never conflated. Never add a required field to an existing proto message. Never make a manifest field's absence silently default to something undocumented — either it's optional with a documented default in `docs/04-plugin-protocol-spec.md` §3's table, or it's required and absence is a hard startup error naming the file and field.
4. **The kernel never blocks its own async runtime.** No `std::thread::sleep`, no synchronous file I/O, no synchronous network calls anywhere inside code that runs on a Tokio worker thread (`forge-core`, `forge-gateway`). If you need blocking work, you reach for `tokio::task::spawn_blocking` or you push the problem to a plugin's own process, never into the kernel's runtime threads.
5. **Forge makes zero required outbound network calls at runtime beyond what the operator explicitly configured.** No telemetry, no update checks, no "phoning home." This is tested, not just promised — PRD §6 success criterion 7.

---

## 2. Build Order (do not reorder this without telling the user why)

This order exists because each step's interface is a hard dependency for the next, and skipping ahead means you'll be guessing at a contract that's actually already fully specified two documents back.

1. **`proto/forge_plugin_v1.proto`** — copy the file at `docs/forge_plugin_v1.proto` verbatim into the `proto/` directory described in Build & Distribution Spec §2. Do not modify a single field. This file has already been validated to compile with `grpc_tools.protoc` — if your toolchain fails to compile it, the problem is your toolchain setup, not the file.
2. **`crates/forge-proto`** — wire up `tonic-build`/`prost-build` against that proto file. Confirm `cargo build -p forge-proto` succeeds before touching anything else.
3. **`crates/forge-core`** — implement, in this exact sub-order, per Architecture Spec §2:
   a. `lifecycle` — the state machine from §2.1 (`DISCOVERED → CONNECTING → HANDSHAKING → READY → DEGRADED → DRAINING → STOPPED`). Write this as an explicit `enum` with an explicit transition function that rejects illegal transitions (e.g., `STOPPED → READY` directly is illegal — it must re-enter at `DISCOVERED`). Do not let this become an implicit set of booleans.
   b. `registry` — the capability map from §2.2. Implement `register`, `deregister`, `lookup`, `list_capabilities` exactly as named there. Concurrency: `tokio::sync::RwLock` or `dashmap` — your choice, but reads must not block reads.
   c. `bus` — `dispatch()` from §2.3, with the exact four-step sequence (resolve → deadline check → send+await with timeout → return typed result). Use the request-ID correlation pattern from §4, not assumed message ordering.
   d. `config` — manifest discovery + `forge.toml` loading, precedence order from §2.4 (CLI > env `FORGE_*` > manifest/config file > defaults).
4. **`crates/forge-gateway`** — gRPC (`tonic`) and HTTP (`axum`) listeners per §2.5. This module is translation-only: decode wire → build `Invocation` → call `bus::dispatch` → encode response. If you find yourself writing logic here that isn't transport encoding/decoding, that logic belongs in a plugin — go back to rule #1.
5. **`crates/forge-cli`** — the `forge` binary: `forge run`, `forge status`, `forge plugin restart <name>`, exactly the commands shown in Operator's Guide §6.
6. **First plugin, to validate the whole loop**: implement the Rust `echo-rs` example from Plugin Developer's Guide §2, verbatim, before attempting any "real" plugin. If `echo-rs` doesn't round-trip through `curl` exactly as shown in §2.5 of that doc, do not proceed to anything else — debug this first, since every later plugin depends on this same path working.
7. Only after step 6 passes: build the official plugins (`forge-plugin-http-router`, `forge-plugin-auth-jwt`, `forge-plugin-data-sqlite`) per Plugin Developer's Guide §7, then the Python `echo-py` example (§3) to prove cross-language compatibility before declaring any milestone "done."

**Do not parallelize across these steps in a way that has you writing `forge-gateway` before `forge-core`'s `bus::dispatch` actually compiles and has a test passing against it.** Sequential, verified, then move on.

---

## 3. Per-Session Checklist (run through this at the START of every session, even if you "remember" the last one)

- [ ] Did I read docs 01–05 fresh this session (§0 above)? If this is a long session and context may have drifted, re-read the specific document section relevant to what I'm about to touch.
- [ ] Am I about to add ANY logic to `forge-core` or `forge-gateway` that knows about HTTP paths, auth schemes, or data storage? If yes — STOP, that's a plugin, not kernel code (rule #1).
- [ ] Am I about to add a new RPC method, a new required proto field, or change an existing field's number/type? If yes — STOP, check TRD §7 and Plugin Protocol Spec §8's versioning rules first, and tell the user this is a breaking change before doing it.
- [ ] Does my code compile AND pass whatever tests exist before I move to the next build-order step in §2 above?
- [ ] Have I introduced any blocking call inside async kernel code (rule #4)? Grep for `std::thread::sleep`, `std::fs::` (non-tokio), `std::net::` (non-tokio) inside `forge-core`/`forge-gateway` before finishing the session.
- [ ] Have I added any network call, telemetry, or "phone home" the user didn't explicitly ask for (rule #5)? If yes, remove it.

---

## 4. What "Done" Means for Any Single Piece of Work

Do not mark a module complete just because it compiles. Per PRD §6, a piece of work is done when:
- It matches its specification section in the relevant document exactly — re-read that section and check field-by-field, state-by-state.
- Killing/crashing a plugin mid-invocation behaves exactly per Architecture Spec §5's failure semantics — not "probably fine," actually test it by killing a plugin process mid-request and confirming the caller gets `TransportError`, not a hang.
- Nothing about it requires reading Rust source to operate (if it's operator-facing) or requires touching kernel code (if it's plugin-facing) — re-confirm against PRD §6 success criteria 1–7 directly.

---

## 5. When You're Unsure

If a document doesn't answer your exact question, do not guess silently and do not invent new architecture. Instead:
1. Quote the most relevant section you found.
2. State plainly what's ambiguous.
3. Propose the smallest-deviation answer consistent with the Five Sentences in §1.
4. Ask the user to confirm before writing code against that assumption.

This rule exists specifically because free-tier/smaller models tend to paper over ambiguity with a confident-sounding invention rather than flagging it — flagging it is the correct behavior here, every time, even if it feels like it slows you down.

---

## 6. Anti-Drift Reminder

If you notice yourself, partway through a session, starting to:
- merge two of the five kernel modules together "for simplicity,"
- add a REST-specific or auth-specific shortcut directly into `forge-gateway`,
- treat the official plugins as special-cased / privileged in the kernel code,
- skip the manifest/proto version checks "since it's just a demo,"
- or introduce a second wire format alongside gRPC+HTTP "to make one plugin easier,"

— stop immediately, re-read Architecture Spec §7 ("What This Architecture Deliberately Does Not Decide") and TRD §5 (microkernel minimality), and report to the user that you almost drifted and why, before continuing.
