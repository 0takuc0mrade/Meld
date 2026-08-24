# Meld Architecture Decisions

## ADR-001 — One process and one crate for the MVP

**Decision:** Build one Axum binary crate containing the domain, supervisor, worker adapters, verifier, API, and embedded/static web assets.

**Context:** The demo window is three days. Meld needs one authoritative control plane, not distributed scale.

**Options considered:** A single crate; a Cargo workspace split into core/server/frontend crates; several services with an external queue.

**Chosen approach:** One process and one crate with strong Rust module boundaries.

**Why:** It minimizes deployment and dependency complexity while retaining testable boundaries.

**Tradeoffs:** A process crash loses in-memory state, and modules are not separately versioned.

**At larger scale:** Extract a core crate only when another binary genuinely consumes it; add durable storage before multiple server replicas.

## ADR-002 — Explicit enum state machine

**Decision:** Represent task state as a Rust enum and route every mutation through supervisor transition methods.

**Context:** Workers are untrusted producers of candidate output. Invalid or contradictory state must be difficult to represent.

**Options considered:** Status strings; a record with booleans; an enum with state-specific fields; a full state-machine framework.

**Chosen approach:** A handwritten enum and exhaustive `match` statements.

**Why:** It is explicit, dependency-free, readable, and compiler-checked.

**Tradeoffs:** Transition code is somewhat repetitive, and persistence migrations will later need care.

**At larger scale:** Keep the domain enum; add versioned persistence events or records rather than introducing a framework solely for syntax.

## ADR-003 — Assignment ID plus monotonic generation

**Decision:** Every assignment receives a unique ID and a task-local monotonically increasing generation. Submissions must include both.

**Context:** A late Worker A must not overwrite Worker B's accepted result.

**Options considered:** Worker ID only; deadline only; random lease token only; task-local generation only; assignment ID plus generation.

**Chosen approach:** Match task ID, assignment ID, and generation against the active assignment before verification.

**Why:** Generation makes reassignment history obvious and testable; assignment ID distinguishes separate leases; the combination gives clear diagnostics.

**Tradeoffs:** Tokens are slightly larger and remote workers will later require authentication.

**At larger scale:** Use unguessable signed/scoped capability tokens and persistent atomic generation increments, while retaining the same semantic check.

## ADR-004 — Deadline-based detection before heartbeats

**Decision:** The MVP detects an unresponsive worker when its assignment deadline elapses.

**Context:** The demo must perform real failure detection, but heartbeat renewal adds a protocol and more race conditions.

**Options considered:** Frontend timer; backend fixed deadline; periodic heartbeats; OS process supervision.

**Chosen approach:** A backend Tokio deadline task tied to an assignment token.

**Why:** It is a real liveness mechanism for bounded tasks and is deterministic under Tokio's test clock.

**Tradeoffs:** Failure is not detected until the deadline, and legitimately long tasks need a suitable lease.

**At larger scale:** Add authenticated heartbeat/lease renewal and worker health signals, with maximum lease and retry policies.

## ADR-005 — `Mutex` rather than `RwLock` or an actor command loop

**Decision:** Protect the small in-memory store with `tokio::sync::Mutex` and call supervisor methods directly.

**Context:** Deadline and completion paths race and require atomic check-and-transition behavior.

**Options considered:** `std::sync::Mutex`; Tokio mutex; Tokio `RwLock`; a single-owner actor with `mpsc` and `oneshot` commands.

**Chosen approach:** Tokio mutex, held only for short state operations.

**Why:** It is the smallest understandable concurrency model. Most operations mutate after reading, so `RwLock` gives little benefit. An actor loop adds message plumbing and shutdown/backpressure decisions.

**Tradeoffs:** All state operations serialize. Holding the guard across `.await` would be dangerous, so code review must enforce short lock scope.

**At larger scale:** Measure first. Durable database transactions or a partitioned actor/store may replace the mutex when real contention or multiple replicas exist.

## ADR-006 — Broadcast channel only for events

**Decision:** Use `tokio::sync::broadcast` to fan committed events out to SSE clients; do not use channels as authoritative storage.

**Context:** UI clients need prompt live updates, but the state mutation path should remain direct and testable.

**Options considered:** Polling; one `mpsc` per client; broadcast; WebSockets; an external broker.

**Chosen approach:** Broadcast plus a bounded in-memory event history and snapshot endpoint.

**Why:** It decouples observers and naturally supports multiple subscribers with little code.

**Tradeoffs:** Slow subscribers can miss events and must recover from a snapshot/history.

**At larger scale:** Persist events and use a durable broker only if cross-process fan-out is actually required.

## ADR-007 — SSE rather than WebSockets or polling

**Decision:** Stream backend events to the browser with Server-Sent Events.

**Context:** After the user starts a mission, data flows primarily from server to browser.

**Options considered:** Interval polling; long polling; SSE; WebSockets.

**Chosen approach:** SSE for live events, normal HTTP for commands and snapshots.

**Why:** Native browser reconnection, simple text/event framing, and a one-way semantic match.

**Tradeoffs:** It is not a general bidirectional transport, and lag/reconnect needs sequence-aware recovery.

**At larger scale:** WebSockets may be justified for interactive remote-worker protocols, not for this read-only execution story.

## ADR-008 — Deterministic verifier owns acceptance

**Decision:** The initial verifier applies typed deterministic rules. A model does not authoritatively approve another model.

**Context:** “Verified” must mean more than an LLM assertion.

**Options considered:** Trust worker output; use a second LLM as judge; deterministic schema and acceptance rules; human approval.

**Chosen approach:** Deterministic rules appropriate to the narrowly defined demo mission.

**Why:** The result is reproducible, explainable, and testable.

**Tradeoffs:** Deterministic checks cannot prove broad semantic truth. Product copy must describe exactly what was checked.

**At larger scale:** Combine deterministic policy, provenance, sandboxed checks, model scoring, and human gates; keep final authority in explicit policy.

## ADR-009 — Rig is an adapter, feature-gated and delayed

**Decision:** Implement and test the core without Rig, then add a Rig-backed `Worker` behind a Cargo feature after a dependency review.

**Context:** Rig is useful for model integration but must not own reliability logic. It also introduces a larger, changing transitive supply chain.

**Options considered:** Make Rig central; call one provider SDK directly; isolate Rig behind a trait; omit models entirely.

**Chosen approach:** Narrow adapter and one provider at most.

**Why:** The core stays deterministic and testable offline, while the demo can still run real agent work.

**Tradeoffs:** Feature-gating adds a build variant, and the adapter work happens later.

**At larger scale:** Add providers deliberately, each with scoped credentials, timeouts, rate limits, and provenance. Avoid a generic plugin system until needed.

## ADR-010 — Browser-native frontend

**Decision:** Use HTML, CSS, and a small JavaScript ES module instead of a frontend framework for the MVP.

**Context:** The product is one execution screen, while supply-chain exposure and build reliability matter.

**Options considered:** React/Vite; another SPA framework; server-rendered templates; browser-native assets.

**Chosen approach:** Static browser-native files served by Axum.

**Why:** No Node package graph or separate build pipeline is required, and the UI remains capable of polished CSS, SSE, and accessible interactions.

**Tradeoffs:** State updates and component organization are manual. This is acceptable for one screen.

**At larger scale:** Adopt a framework if routes, reusable components, and complex local interactions justify its cost.

## ADR-011 — Minimal dependency and source policy

**Decision:** Every crate addition requires a source/feature/transitive review; crates.io is the only default source; Git dependencies are rejected unless explicitly reviewed and pinned; the application lockfile is committed.

**Context:** Rust packages can execute code through build scripts and proc macros during a build. Advisory scanning alone does not prevent a malicious new release or compromised maintainer.

**Options considered:** Trust semver ranges and CI; use advisory scans only; vendor all sources immediately; layered dependency controls.

**Chosen approach:** Layered controls detailed in `SUPPLY_CHAIN_SECURITY.md`, with vendoring postponed until the graph stabilizes.

**Why:** It addresses provenance, change review, build-time execution, and reproducibility without making the three-day MVP impossible.

**Tradeoffs:** Reviews take time and scanners can report noise. A lockfile pins bytes by checksum but is not proof that code is benevolent.

**At larger scale:** Add `cargo-vet` audits/criteria, signed release provenance, an internal registry or reviewed vendor directory, SBOM generation, and isolated reproducible builders.

## ADR-012 — Boxed standard futures for object-safe workers

**Decision:** Start with a `Worker` trait that returns `Pin<Box<dyn Future<...>>>` rather than adding `async-trait`.

**Context:** The worker registry needs heterogeneous implementations. Native `async fn` in traits is not directly usable as a trait object in the required shape.

**Options considered:** A generic supervisor; an enum of worker implementations; `async-trait`; a manually boxed future.

**Chosen approach:** Manually boxed future with a local type alias.

**Why:** It avoids another proc macro in the trusted build path and keeps the dynamic boundary explicit.

**Tradeoffs:** The signature introduces `Pin` and trait-object concepts earlier than ideal for a learner.

**At larger scale:** Reconsider based on ergonomics and measured need. Adding a carefully vetted `async-trait` crate is acceptable if it materially improves maintainability.

## ADR-013 — Keep both sides of the deadline race alive

**Decision:** Spawn independent worker and deadline tasks. Detach the loser rather than canceling it immediately.

**Context:** Meld must prove both that a post-completion deadline is harmless and that a post-expiry worker result is stale. Remote work may also ignore local cancellation.

**Options considered:** `timeout(worker_future)` and cancel on expiry; abort the losing task; keep both tasks alive with token-checked supervisor calls.

**Chosen approach:** `run_task` observes both `JoinHandle`s with `tokio::select!`. The worker task itself submits its outcome before completing. The deadline task itself calls `expire_assignment`. Dropping a handle detaches rather than aborts its task.

**Why:** Correctness comes from authoritative token validation, not optimistic cancellation. Both race orders are exercised by real code.

**Tradeoffs:** Detached tasks need lifecycle accounting and a cancellation grace period in a long-running server. Phase 1 uses short controlled workers and bounded tests.

**At larger scale:** Track in-flight tasks in a `JoinSet` or cancellation registry for graceful shutdown and resource control, while retaining token checks as the safety mechanism.

## ADR-014 — Verification is synchronous and outside the state lock

**Decision:** `submit_result` transitions `Running -> Verifying` under the mutex, releases it, runs `Verifier::verify`, then reacquires it for `Verifying -> Completed/Recovering`.

**Context:** Even a deterministic verifier may later perform costly checks. It must not serialize unrelated state operations.

**Options considered:** Verify inside the critical section; verify first; use the two-transition protocol.

**Chosen approach:** Two transitions with a typed `Submission` linking them.

**Why:** A result becomes the one authoritative candidate before verification, while the mutex stays short-lived.

**Tradeoffs:** The task is observably `Verifying` between calls, so `record_verification` must revalidate the submission ID and token.

**At larger scale:** Run expensive verifiers in bounded tasks or isolated services; preserve the same candidate identity check.

## ADR-015 — Exact direct versions and a 17-package Phase 1 graph

**Decision:** Pin the four direct crates exactly and commit `Cargo.lock`; omit Serde and ANSI logging until needed.

**Context:** The user explicitly called out Rust supply-chain attacks, and Phase 1 needs a small trusted build surface.

**Options considered:** Normal compatible version ranges; exact direct versions; vendoring every source immediately.

**Chosen approach:** Exact direct versions plus checksum-locked crates.io transitives and manual/build-script/RustSec review.

**Why:** It makes intentional upgrades obvious and kept the complete graph to 17 registry packages.

**Tradeoffs:** Security updates require an explicit manifest edit as well as a lockfile update. Vendoring and third-party criteria audits remain undone.

**At larger scale:** Automate reviewed update PRs and add `cargo-deny`, `cargo-vet`, SBOMs, and isolated builders.

## ADR-016 — Workspace-local Git metadata workaround

**Status:** Superseded after the user successfully initialized a normal writable `.git` repository.

**Decision:** Use `.git-local` as Git's metadata directory for this managed workspace and ignore it from the work tree.

**Context:** The environment mounts an empty `.git` read-only and reports it as a busy mount point, so standard initialization is impossible without control of the workspace mount.

**Options considered:** Delete documentation and clone elsewhere; nest the application; proceed without reviewable Git state; use an alternate Git directory.

**Chosen approach:** Configure a bare `.git-local` with `core.worktree = ..`, branch `main`, and the supplied GitHub origin.

**Why:** It preserves the requested root layout and provides status/diff support without deleting files.

**Tradeoffs:** Commands need `GIT_DIR=.git-local` in this environment, and normal Git clients will still see the broken mounted `.git` first.

**At larger scale:** Use a normal clone where metadata is at `.git`; `.git-local` should not be copied or committed.

**Resolution:** The standard repository now uses branch `main` with `origin` set to `https://github.com/0takuc0mrade/Meld.git`. `.git-local` is obsolete and ignored.
