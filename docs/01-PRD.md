# Forge — Product Requirements Document

**Document 1 of 7 — Foundational**
**Status:** Final
**Owner:** Daksh
**Depends on:** Nothing (this is the root document)
**Read before:** every other document in this suite

---

## 1. What Forge Is

Forge is a **polyglot backend microkernel**. It is not a web framework, not an ORM, not an API gateway — it is the infrastructure substrate those things get built out of. Forge itself does almost nothing. What it does is provide a stable, versioned contract by which independent pieces of backend logic — written in any language, running in any process, on any machine — register themselves, advertise what they can do, and get orchestrated into a working backend.

The analogy worth holding in your head: Forge is to a backend what a kernel is to an OS. A kernel does not know what a text editor is. It provides processes, memory, scheduling, and a syscall boundary, and text editors get built on top using that boundary. Forge does not know what "user authentication" is. It provides plugin lifecycle, a capability registry, message routing, and a wire protocol, and authentication gets built on top as a plugin using that boundary.

This document defines what Forge must be true to, regardless of which project later uses it.

---

## 2. Why This Exists

You (Daksh) are building a portfolio aimed at systems software roles — networking, embedded, low-level — for 2027 internship cycles at top-tier companies. A single polished kernel project (OPERtur) and a single polished app project (mapit) are strong signals, but they are both *vertical*: one shows you can build a kernel, one shows you can build a tool. Neither shows that you can design **infrastructure that other engineers and other projects build on top of** — which is a distinct and highly valued skill (it's what separates "wrote an app" from "designed a platform").

Forge is meant to be the third pillar: proof that you can design a piece of reusable infrastructure with a real ABI/protocol contract, ship it as installable software, and document it to a standard where a stranger (or an AI agent) can extend it without talking to you. The deliverable is not just code — it is the **discipline of the contract**: versioning, backward compatibility, failure semantics, and documentation good enough to survive contact with someone else's use case.

Because you don't know in advance whether the showcase use will be an embedded sensor service, a security tool, or a conventional web backend, Forge cannot bake in assumptions about any of those. That constraint is the spine of the entire design.

---

## 3. Who This Is For

Two audiences, and the documentation suite has to serve both without compromising either:

**Audience A — Operators.** Someone who wants a backend up and running fast: install Forge, point it at a directory of plugins or a config file, run it, get a working backend. They should never need to read Rust to use Forge. Served by Document 6 (Operator's Guide).

**Audience B — Plugin Developers.** Someone (possibly you, six months from now, possibly an AI coding agent, possibly a hypothetical other engineer) extending Forge by writing a plugin — in Rust, Python, Go, C, or anything else that can speak gRPC or HTTP. They need the protocol contract, the manifest schema, and a working example in at least two languages. Served by Documents 4 and 7.

A third, implicit audience is **future-Daksh auditing this project for an interview**. The documentation must stand on its own as evidence of design thinking — an interviewer reading Document 3 should come away convinced the architecture was deliberate, not accidental.

---

## 4. Core Design Principles (non-negotiable, referenced by every later document)

These principles resolve every ambiguous design decision in this suite. When a later document seems to leave a choice open, return here.

1. **Mechanism, not policy.** The kernel provides *how* plugins communicate, register, and get invoked. It never provides *what* they do. Routing is not a kernel feature — it's a plugin. Auth is not a kernel feature — it's a plugin. If you find yourself adding a feature to the kernel because "every backend needs this," stop — that feature is a plugin, possibly a default/bundled one, but never kernel code.

2. **The protocol is the product.** The Rust code is one implementation of the contract. The contract — manifest schema, gRPC/HTTP surface, lifecycle states, versioning rules — is what actually matters, because it's what makes "any language" true. If the Rust implementation and the protocol spec ever disagree, the protocol spec wins and the implementation is bugged.

3. **No assumed deployment shape.** Forge must run identically as: a single static binary an operator downloads and runs, a Rust crate embedded inside a larger Rust program, and a long-running daemon spoken to over a socket. None of these is the "real" mode with the others bolted on — they are three faces of one core.

4. **Degrade gracefully toward zero.** On a constrained target (embedded), Forge with zero plugins loaded must still start, hold a minimal footprint, and do nothing harmful. There is no minimum viable feature set beyond "the kernel itself boots and the registry is queryable." A web server, a database connection pool, a router — none of these may be assumed present.

5. **Versioned or it doesn't exist.** Every contract surface (manifest schema, wire protocol, capability interface) is versioned from day one, with an explicit compatibility policy. Adding a field to a message without a version bump is a defect, not a convenience.

6. **Documentation is load-bearing.** A feature without a corresponding section in the relevant document is not considered complete. This suite exists specifically so nothing is left for a user (human or agent) to infer.

---

## 5. Scope

### 5.1 In scope (v-final, as you specified — no "v1, fix later")

- A Rust-implemented kernel binary/crate/daemon (the "core") handling: process/plugin lifecycle, capability registry, an internal async message bus, configuration loading, and a dual gRPC+HTTP gateway for external and plugin communication.
- A formally specified plugin protocol (Document 4) covering manifest format, handshake/registration sequence, capability declaration, invocation semantics, health/lifecycle signaling, and versioning rules.
- Reference plugin implementations in **at least two languages** (Rust and one dynamic language — Python is recommended, see Document 7) demonstrating the protocol end-to-end, including at least: one routing/HTTP-surface plugin, one data-persistence plugin, one auth/middleware-style plugin.
- A single-binary distribution path (installer script + prebuilt binary per target triple) and a crates.io-publishable-shape crate, even if not actually published.
- Full documentation suite (this set of 7 documents) plus the two step-by-step guides built on top of them.
- A working example backend assembled purely from plugins, runnable end-to-end, demonstrating at least a REST-style CRUD surface backed by a plugin-provided data layer and a plugin-provided auth check.
- A minimal/embedded build profile demonstrated on a constrained target conceptually compatible with your ESP32 self-study track (the documentation must describe this even if full ESP32 wiring is a stretch goal — see Document 5, §7).

### 5.2 Out of scope (explicitly — revisit only with a new PRD revision)

- A plugin marketplace, registry server, or signing/distribution infrastructure beyond "plugins are files/binaries you point Forge at." (Future work, not v-final.)
- A built-in ORM, query language, or schema migration tool. Data plugins may wrap existing ones (e.g., a plugin that wraps `sqlx`), but Forge does not ship one.
- A built-in UI/admin dashboard. (A plugin could provide one later; not core.)
- Multi-tenant SaaS control-plane features (billing, org management). This is infrastructure, not a hosted product.
- Hot-reloading of the kernel itself. Plugin hot-reload is in scope (Document 3, §6); kernel hot-reload is not.
- Windows as a first-class target. Linux is primary (matches your Fedora environment); macOS is best-effort; Windows is documented as "should work via WSL2, untested natively."

---

## 6. Success Criteria — what "done" actually means

Forge is complete when all of the following are true simultaneously:

1. A person with no prior context can run one install command on a fresh Fedora (or Ubuntu) machine and have a Forge binary on their `$PATH` within two minutes, using only Document 6.
2. That person can write a manifest file and a plugin in Python (zero Rust knowledge required) that registers a single HTTP route, following only Document 7, and see it served by Forge without editing any Forge source.
3. Someone fluent in Rust but new to Forge can embed the Forge core as a crate inside their own `main.rs`, following only Document 6 §[embedding], and get a running instance in under 20 lines of code.
4. The example backend (§5.1) runs with `forge run` and correctly serves requests, demonstrating cross-plugin interaction (the auth plugin gates the data plugin's routes) purely through the documented protocol — no special-cased glue code in the kernel.
5. Killing a single plugin process does not crash the kernel or other plugins; the kernel's behavior in that situation is exactly what Document 3 §[failure semantics] says it will be.
6. Every wire message, manifest field, and lifecycle state mentioned in Document 3 or 4 has no undocumented behavior — an auditor cross-referencing code against docs finds zero gaps.
7. The whole system builds and the example backend runs entirely offline after initial dependency fetch (no required external network calls at runtime), which matters for the security-showcase and embedded use cases.

---

## 7. Representative Use Cases (these drive design decisions in Documents 3–5; they are illustrative, not exhaustive — point 4 of §4 forbids designing only for these)

- **Web backend showcase:** Forge running as a daemon, gRPC+HTTP gateway exposed, plugins providing REST routes, JWT auth, and Postgres persistence — a conventional-looking backend assembled from independently testable, swappable pieces, useful for demonstrating clean architecture in interviews.
- **Security tool showcase:** Forge embedded as a crate inside a CLI tool, zero network gateway exposed, plugins doing packet inspection or log analysis, communicating over the internal bus only — demonstrating that "backend framework" doesn't require "web server."
- **Embedded showcase:** Forge's kernel compiled in a minimal profile, a single plugin reporting sensor data over the gRPC gateway to a separate aggregator, demonstrating the framework scales down, tying directly into your ESP32 self-study track.
- **Tooling/demo showcase:** Forge running ad hoc via `forge run --plugins ./demo-plugins` for a five-minute interview demo, showing the registry, the live plugin list, and a request flowing through two plugins — fast to spin up, easy to narrate.

---

## 8. Constraints Carried From Your Environment (informs later documents, recorded here so they aren't forgotten)

- Primary development and target OS: Linux (Fedora), x86-64 (Ryzen 7 7735HS) — no Apple Silicon–specific assumptions.
- You have an existing, validated pattern (`mapit`) of: Rust core, pluggable provider adapters, local-first defaults. Forge should feel like a generalization of that pattern, not a different philosophy.
- You are simultaneously deep in OS-level work (OPERtur) involving raw networking (E1000 driver, TCP/IP stack by hand). Forge's gRPC/HTTP gateway is explicitly **not** a place to reinvent that work — use established Rust crates (`tonic` for gRPC, `axum` or `hyper` for HTTP) rather than hand-rolling transport. The systems-programming showcase already lives in OPERtur; Forge's job is to showcase *architecture*, not to duplicate the NIC-driver-level showcase.
- Toolchain assumed available: stable Rust via `rustup`, `cargo`, standard crates.io access — consistent with your existing workstation setup.

---

## 9. Document Map (the rest of this suite)

| # | Document | Answers |
|---|----------|---------|
| 1 | PRD (this document) | What and why |
| 2 | Technical Requirements Document | Hard technical contracts: language, async model, performance/safety budgets |
| 3 | Architecture Specification | How the kernel itself is structured internally |
| 4 | Plugin Protocol Specification | The wire contract that makes "any language" real |
| 5 | Build & Distribution Specification | How it compiles, packages, and installs in three shapes |
| 6 | Operator's Guide | Step-by-step: install, configure, run, observe, upgrade |
| 7 | Plugin Developer's Guide | Step-by-step: write, register, test, ship a plugin in any language |

Read in order on first pass. After that, each is self-contained enough to be referenced independently.
