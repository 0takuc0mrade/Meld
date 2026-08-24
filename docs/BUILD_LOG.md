# Meld Build Log

This is a chronological engineering record. It explains what changed and why, not just what a commit contained.

## 2026-08-24 — Phase 0: repository inspection and MVP architecture

### What was added

- Initial architecture and lifecycle design in `docs/ARCHITECTURE.md`.
- Initial Rust study notes in `docs/RUST_LEARNING.md`.
- Initial architectural decision records in `docs/DECISIONS.md`.
- A dependency and build-integrity policy in `docs/SUPPLY_CHAIN_SECURITY.md`.
- This build log.

No application code, manifest, dependency, or frontend asset was added in this phase. That is intentional: the brief asks for architecture review before Phase 1 implementation.

### Environment inspected

- The repository contains no existing application files or Cargo workspace.
- The `.git` directory is present but does not contain a usable Git repository, so `git status` cannot currently report changes.
- Installed toolchain: `rustc 1.95.0` and `cargo 1.95.0`.
- No `cargo-audit`, `cargo-deny`, or `cargo-vet` executable was found on `PATH`.

The missing Git metadata matters because a committed `Cargo.lock` and reviewable lockfile diffs are part of the proposed supply-chain defense. Before dependencies are introduced, the repository should be initialized or repaired so lockfile changes can be reviewed.

### Understanding of Meld

Meld is a deterministic Rust control plane around nondeterministic workers. It owns the authoritative task lifecycle, creates expiring assignments, detects a late or failed worker, reassigns the same work under a new generation, verifies candidate output, and accepts completion only through its state machine. Workers can produce outputs, but they cannot mutate task state.

The demo may deliberately make the first worker return after its lease deadline. Everything after that injected lateness—deadline detection, lease invalidation, reassignment, verification, completion, and stale-result rejection—must run through normal production code and produce the same events the UI consumes.

### Chosen MVP shape

The proposed MVP is one Rust binary with:

- an Axum HTTP API and static frontend;
- an in-memory authoritative store protected by `Arc<tokio::sync::Mutex<_>>`;
- a supervisor service containing all allowed state transitions;
- worker and verifier traits that do not depend on Rig;
- Tokio tasks for concurrent worker execution and lease deadlines;
- a Tokio broadcast channel for read-only event subscribers;
- Server-Sent Events (SSE) for the browser timeline;
- a dependency-free browser frontend using semantic HTML, CSS, and a small ES module.

Persistence, distributed coordination, user authentication, multiple model providers, and a general plugin system are explicitly postponed.

### Why this solves the immediate problem

This structure is small enough to implement and explain in three days, but the important behavior is real:

1. The supervisor creates a task and issues generation 1.
2. Worker A runs in its own Tokio task.
3. A separate deadline task expires generation 1 if it is still current.
4. The supervisor issues generation 2 to Worker B.
5. Worker B submits generation 2 and the verifier accepts it.
6. Worker A can still return; its generation-1 token is rejected by the same submission path.
7. Each accepted transition appends a backend event and publishes it to SSE.

The browser has no timers that advance task state and no recovery logic.

### Supply-chain posture established

The user explicitly called out Rust supply-chain attacks. The initial policy is therefore stricter than simply running `cargo audit`:

- keep the direct dependency set small;
- accept crates.io dependencies only by default;
- prohibit unreviewed Git dependencies;
- commit `Cargo.lock` and build with `--locked`;
- pin the Rust toolchain used for the demo;
- inspect every dependency addition, including features, build scripts, proc macros, native code, and transitive changes;
- add automated advisory, license, duplicate-version, and source checks;
- prepare and run the final demo from prefetched dependencies with network access disabled where practical;
- treat Rig as an optional, separately reviewed adapter rather than part of the trusted state machine.

See `docs/SUPPLY_CHAIN_SECURITY.md` for the review checklist and incident response.

### Problems encountered

The workspace has a `.git` directory, but Git reports that the directory is not a repository. No destructive or speculative repair was attempted because the initial task is architecture-only and repository ownership is unclear.

### Files changed

- `docs/BUILD_LOG.md`
- `docs/ARCHITECTURE.md`
- `docs/RUST_LEARNING.md`
- `docs/DECISIONS.md`
- `docs/SUPPLY_CHAIN_SECURITY.md`

### What remains unfinished

- Review and approve this architecture.
- Repair or initialize repository metadata.
- Scaffold the Cargo workspace and pin the toolchain.
- Add and vet the first dependency set.
- Implement and test the pure domain state machine.
- Implement the supervisor, deadlines, workers, verifier, API, SSE, and UI.
- Add an optional Rig-backed worker only after its dependency tree is reviewed.
- Rehearse the two-minute demo and failure path offline.

## 2026-08-24 — Phase 1: deterministic reliability core

### Git repair

The pre-existing `.git` path is an empty read-only workspace mount. Git could not initialize it, changing its mode from the escalated environment could not see it, and removing it returned `Device or resource busy`. No project file was removed.

To preserve reviewable changes in this environment, a bare metadata directory was initialized at `.git-local`, configured with the Meld root as its work tree, renamed to branch `main`, and connected to `https://github.com/0takuc0mrade/Meld.git`. `.git-local/` is ignored. Commands in this workspace use `GIT_DIR=.git-local git ...`. On a normal checkout, the metadata should live at the standard `.git` path.

Later the same day, the user successfully ran `git init` directly from their terminal, replacing the managed mount with a normal writable `.git`. The standard repository was then renamed to branch `main` and configured with the same GitHub origin. Normal `git status` now works. `.git-local` is obsolete, remains ignored, and was not deleted without explicit approval.

### Crate scaffold and dependencies

Added one Rust 2024 package, pinned to Rust 1.95.0 through `rust-toolchain.toml`. Direct dependencies are exact versions:

- `tokio 1.53.1`: runtime, tasks, monotonic time, mutex, broadcast, `select!`, and controlled test time;
- `thiserror 2.0.20`: typed error implementations;
- `tracing 0.1.44`: structured lifecycle records;
- `tracing-subscriber 0.3.23`: minimal binary log formatter.

Serde was deferred because Phase 1 has no serialization boundary. Axum and Rig were not added.

The first compile failed because the global Cargo registry cache is a read-only mount. Builds were rerun with the task-scoped cache `/tmp/meld-cargo`. During that failed attempt, inspection showed the `ansi` tracing-subscriber feature would add `nu-ansi-term`; ANSI output was unnecessary, so the feature was removed. This reduced the locked registry graph from 20 to 17 packages.

### Domain and state machine

Added typed task, assignment, submission, generation, worker, mission, output, failure, verification, and rejection types. `TaskState` is an enum with `Pending`, `Assigned`, `Running`, `Recovering`, `Verifying`, `Completed`, and `Failed` variants. State-specific data lives inside each variant.

The state store and its mutex are private. Only `Supervisor` methods mutate it. Every operation validates state/token and commits events under the mutex, releases the guard, then traces and broadcasts the committed events.

### Workers, deadlines, and recovery

Added the object-safe `Worker` trait plus `SuccessfulWorker`, `ErrorWorker`, `PanicWorker`, and `ControlledDelayWorker<W>`. The delay wrapper runs the inner worker first and withholds its result afterward, so it can later wrap a genuine Rig worker without introducing a supervisor demo branch.

Each assignment starts one worker task and one deadline task. `tokio::select!` observes which handle finishes first:

- when the worker wins, the deadline task is detached and later performs a harmless token check;
- when the deadline wins, the worker task is detached and later submits through the normal path.

The store lock serializes the decisive token check. The code never sleeps, executes a worker, or runs verification while holding the lock.

### Verification and events

Added a synchronous `Verifier` trait and `DeterministicVerifier`. The fixture verifier checks summary length, required terms, and non-empty evidence. It does not claim broader semantic truth.

Added immutable events with process-wide monotonically increasing sequence numbers, a bounded per-task history, broadcast publication, and structured tracing. Outputs and mission text are not placed in tracing fields.

### Tests

Added nine integration tests covering:

- successful verified completion;
- worker error and generation-2 recovery;
- worker panic containment and recovery;
- real monotonic deadline expiry and reassignment;
- generation-1 late return after generation-2 completion;
- proof that stale output never reaches the verifier;
- deterministic verification rejection and retry;
- result-first deadline no-op;
- exact-deadline race safety regardless of winner;
- protection of both terminal states;
- bounded history independent of authoritative state.

Paused Tokio time makes timeout tests complete without wall-clock delay.

### Dependency and supply-chain review

- `Cargo.lock` contains 17 crates.io packages, each with a checksum; there are no Git or alternate-registry sources.
- No duplicate package versions or native `links` dependencies exist.
- Licenses are MIT, Apache-2.0, or Unicode-3.0 combinations.
- Proc macros are limited to `thiserror-impl` and `tokio-macros` (with their standard parsing dependencies).
- Custom build targets are `proc-macro2`, `quote`, and `thiserror`. Their build scripts were inspected: they probe the pinned compiler and write/probe only in Cargo output; none downloads code.
- A pinned `cargo-audit 0.22.2` was built only under `/tmp`. The current RustSec database loaded 1,225 advisories and reported no vulnerabilities in `Cargo.lock`.

### Verification results

- `cargo fmt --all -- --check`: passed after applying rustfmt once.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`: passed.
- `cargo test --locked`: 9 integration tests passed; 0 failed; unit/doc targets also passed.
- The final formatting, Clippy, and locked test pass also succeeded with `CARGO_NET_OFFLINE=true`.
- No network, provider credential, LLM, HTTP server, or browser is required by tests.

### Files added or changed

- `.gitignore`
- `Cargo.toml`
- `Cargo.lock`
- `rust-toolchain.toml`
- `src/lib.rs`
- `src/main.rs`
- `src/domain.rs`
- `src/events.rs`
- `src/supervisor.rs`
- `src/verifier.rs`
- `src/worker.rs`
- `tests/lifecycle.rs`
- all documentation under `docs/`

### Bugs and fixes

- The initial multi-file patch targeted `src/domain.rs` twice and was rejected atomically. The patch was restructured; no partial source remained.
- The global Cargo cache was read-only. A task-scoped cache fixed builds without altering the global toolchain.
- ANSI formatting introduced an unnecessary dependency. Removing that feature reduced the graph.
- Rustfmt initially reported formatting differences. The formatter was applied, then the formatted tree passed checks.

### Deviations and unfinished work

- Git initially required the `.git-local` workaround, but the user subsequently restored a normal `.git`. No commit or push was performed.
- State transitions live in `supervisor.rs`, not a separate `state_machine.rs`, to keep one obvious mutation boundary.
- `tracing-subscriber` is a runtime dependency because the Phase 1 binary initializes readable logs.
- `cargo-deny`/`deny.toml` and `cargo-vet` remain future hardening; source, license, duplicates, native links, build scripts, and RustSec were inspected directly in this phase.
- Graceful shutdown and explicit tracking/draining of all detached tasks belong to the server lifecycle in Phase 2.
- HTTP, SSE, UI, Rig, provider calls, persistence, authentication, and heartbeats remain unimplemented by design.
