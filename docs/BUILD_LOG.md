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

## 2026-08-24 — Phase 2: Axum, SSE, and execution-map frontend

### Dependency review before implementation

The candidate graph was resolved in an isolated `/tmp` manifest before `Cargo.toml` changed. Axum’s `query` feature and a direct `serde_json` dependency were removed because the API has no query input and Axum’s JSON/SSE support already owns serialization internally. The accepted runtime additions are exact versions:

- `axum 0.8.9` with defaults disabled and only `http1`, `json`, `tokio`, and `tracing`;
- `serde 1.0.229` with `derive` for transport DTOs only;
- `tokio-stream 0.1.19` with defaults disabled and only `sync`;
- existing `tokio 1.53.1` gains `net` and `signal`.

Tests directly use the already-transitive `tower 0.5.3` `util` feature and `http-body-util 0.1.5`. The complete lockfile grew from 17 to 60 external crates (43 new). All sources are checksum-locked crates.io registry packages. No Git source, alternate registry, native `links` package, frontend package, or CDN asset was introduced.

### Axum API and application server

Added `src/api.rs` with four product endpoints: health, start demo, task snapshot, and task SSE. The demo endpoint creates a normal mission, wraps two normal `SuccessfulWorker` values in `ControlledDelayWorker`, and hands them to `Supervisor::run_task`. It never mutates task state or publishes an event directly.

Transport DTOs manually map the domain into safe browser fields. API failures use a stable JSON envelope and do not expose debug strings or stack traces. Static files are embedded with `include_str!`, served with explicit content types, and the page receives a local-only Content Security Policy.

`main.rs` now creates the existing supervisor/verifier, binds `127.0.0.1:3000`, serves the Axum router, and handles Ctrl+C graceful HTTP shutdown.

### SSE replay and resynchronization

The SSE handler subscribes to broadcast before taking the task snapshot. It replays retained events after the optional `Last-Event-ID`, then forwards live events. A sequence cursor removes overlap between history and broadcast. Each domain event uses its Meld sequence as the SSE `id`.

Each connection has a bounded 32-item MPSC bridge. If broadcast reports a lagged receiver, the handler emits `event: resync`; the browser refetches snapshot/history. Slow or disconnected clients cannot block the supervisor.

### User-facing frontend

Added a dependency-free, locally served frontend with a Cobalt Map/Diagram design. The primary view is a spatial Worker A → Meld → Worker B → verifier topology rather than a generic dashboard. An event ledger translates backend event kinds into readable sentences; an accepted-result panel proves recovery, reassignment, deterministic acceptance, and stale rejection only after the corresponding events exist.

The frontend stores only the last task ID in `localStorage`. A refresh always requests the authoritative snapshot and history again. JavaScript contains no timeout that advances lifecycle state. The command palette supports Ctrl/Command-K, filtering, arrows, Enter, Escape, backdrop dismissal, and managed focus. The page includes semantic structure, visible focus, ≥44 px controls, icon/label/color status, reduced motion, and layouts for 320, 375, 414, 768, and desktop widths.

### Automated tests

Added seven transport tests covering readiness, demo creation, snapshot retrieval, typed invalid/unknown-task errors, SSE replay and `Last-Event-ID`, complete/stale authority, fresh task histories, and local static/security headers. Paused Tokio time drives the complete deadline/recovery/late-return path without real sleeps.

At the first full Phase 2 test pass, all 7 API tests and all 9 unchanged Phase 1 lifecycle tests passed. Formatting was applied once after rustfmt reported only layout differences.

The pinned `/tmp` `cargo-audit 0.22.2` refreshed a 1,225-advisory RustSec database and reported no vulnerable crate across the 61 lockfile records. The crates.io yanked-version lookup timed out, so the clean advisory scan was rerun with `--no-yanked`; the documentation does not overstate yanked-status coverage.

### Problems encountered and fixes

- `cargo check --locked` correctly refused to proceed before the reviewed dependency graph was written to `Cargo.lock`; the lockfile was generated offline, then locked builds succeeded.
- The managed global Cargo cache was read-only during the isolated graph review. One approved Cargo metadata/download step populated the cache; project builds then ran offline.
- Axum’s default/query features initially pulled unnecessary form/query packages. The feature was removed before the project manifest changed.
- A direct `serde_json` dependency was unnecessary. SSE uses Axum’s `Event::json_data`, keeping Meld’s direct API surface smaller even though JSON remains an Axum transitive.

### Manual validation

The real Axum process was exercised in local headless Chrome through the browser protocol, not a DOM-only test harness. The inspected sequence was:

1. initial idle page;
2. start a fresh mission;
3. Worker A running with SSE connected;
4. full page refresh during Worker A, restoring the same task from snapshot/history;
5. backend lease expiry and recovery;
6. Worker B generation 2 accepted after deterministic policy checks;
7. Worker A’s late generation 1 result rejected;
8. accepted Worker B output and completed state unchanged.

The completed page showed all 13 expected backend events in order. The command palette opened with focus in search and closed by Escape after an explicit cancel-handler fix. Chrome reported no horizontal overflow or wrapped clickable affordance at every 40 px step from 320 through 1920, with explicit checks at 320, 375, 414, 768, and 1440 px. At 1280 × 800, both the headline and primary action fit above the fold. A `prefers-reduced-motion: reduce` browser emulation matched the media query, changed root scrolling to `auto`, and collapsed control transitions. Desktop and 375 px full-page screenshots were inspected; the execution map changes from an asymmetric horizontal topology to a clear vertical flow. A narrow Worker B result label wrapped awkwardly in the first screenshot and was shortened from “Authoritative” to the equally accurate “Accepted.”

Recovery itself is a deliberately short authoritative transition: `run_task` assigns Worker B immediately after expiry. A refresh in that interval reconstructs either `Recovering` or the already-newer generation-2 state plus the retained expiry/reassignment history; it never reconstructs an invented intermediate browser state.

### Phase boundary

Rig and real model providers remain absent. Phase 2 stops after the real backend-controlled browser sequence is validated.

## 2026-08-25 — Phase 3: real Rig workers behind deterministic authority

### Architecture fit

The existing abstractions did not need a rewrite. `RigWorker` implements the Phase 1 `Worker` trait and therefore enters the same `Supervisor::run_task`, assignment-token, deadline, verification, completion, and stale-submission methods as deterministic workers. The existing generic `ControlledDelayWorker<W>` wraps Worker A after its real execution and withholds the completed result; fault injection is not part of Rig or the supervisor.

The Phase 2 endpoint paths, JSON field shapes, event kinds, SSE reconciliation, and frontend source files are unchanged. Health now reports phase 3 in its existing numeric field, and the demo mission data is the incident fixture required for meaningful verification.

### Demo mission and deterministic verification

The fixed mission provides four timestamped checkout incident records. Agents must identify the initiating component, earliest supported onset, evidence record IDs, and a concise summary. Rig extracts those fields into `IncidentAnalysisProposal` using Serde/Schemars.

Structured extraction is not treated as authority. `DeterministicVerifier` requires `payments-api`, onset `2026-08-24T10:01:00Z`, known evidence IDs, and both required records `EV-101`/`EV-102`. Wrong component, unsupported onset, unknown evidence, missing required evidence, or missing incident analysis is rejected through the normal verification recovery path.

### Rig and provider integration

Pinned `rig-core 0.42.0` and `rig-agent 0.42.0` are behind the `rig-worker` feature. The adapter uses one provider—OpenAI—and defaults to `gpt-5-mini`. Its typed extractor has zero model retries and a 400-token maximum. Meld adds its own request timeout and maps provider failure, invalid structured output, or timeout into safe `WorkerError::Execution` values.

Tracing records the agent boundary with provider, model, task ID, assignment ID, generation, and worker ID. It does not record the API key, authorization header, prompt, full response, or environment.

The runtime defaults to deterministic mode. `MELD_EXECUTION_MODE=rig` additionally requires a feature-enabled binary and a non-empty `OPENAI_API_KEY`. The default real timing is a 30-second Meld lease, 20-second provider timeout, and 55-second post-result delay for Worker A. Startup validation rejects timing values that do not leave Worker B enough bounded time to complete before Worker A returns.

### Credentialed smoke status

A scoped replacement OpenAI Platform key was securely created and written only to ignored `.env.local`; no plaintext key was printed or committed. The earlier key whose one-time payload was not captured must be revoked from the OpenAI dashboard.

A real Rig smoke reached OpenAI and authenticated, but the provider returned HTTP `429` with code `insufficient_quota` after approximately eight seconds. This proves credential acceptance and the real HTTP/provider boundary, but it does not prove model output, structured extraction, or the complete two-agent live recovery. Those claims remain blocked until the API project has available credit and spend capacity.

The first failure also exposed an observability issue: Rig's dependency logging emitted the provider's verbose error body and headers before Meld safely classified the returned error. No API key was printed, but provider metadata did appear. `src/main.rs` now filters ordinary tracing to Meld-owned targets. A second credentialed smoke and an Axum mission run showed only safe Meld categories such as `model provider request failed`; dependency response bodies and headers were absent.

### Tests and failure coverage

All existing Phase 1/2 tests remain green. Four incident-policy tests cover acceptance and meaningful rejection. Six feature-gated boundary tests cover valid typed conversion, malformed output, provider failure, provider timeout, generic controlled delay, and a complete two-Rig-worker supervisor run in which generation 2 completes and generation 1 is rejected stale. Tokio's paused clock keeps tests offline, fast, and credit-free.

The mocked complete-recovery test proves control-plane composition but does not make Worker A's late result genuinely AI-generated. In a live run, the wrapper's order is execution first and delay second, so the eventual late result would be genuine provider output.

Final verification passed:

- `cargo fmt --all -- --check`;
- offline all-target/all-feature Clippy with `-D warnings`;
- offline `cargo test --locked`: 20 integration tests passed (7 API, 4 incident verification, 9 lifecycle);
- offline `cargo test --features rig-worker --locked`: the same 20 plus 6 Rig-boundary tests passed;
- feature-disabled `MELD_EXECUTION_MODE=rig` failed clearly because the build capability was absent;
- feature-enabled Rig mode without a key failed clearly with `OPENAI_API_KEY is required when MELD_EXECUTION_MODE=rig`;
- the deterministic Axum binary was started and exercised over HTTP: health reported phase 3, the unchanged demo POST returned `202`, generation 2 completed after deterministic verification, and the final snapshot contained all 13 lifecycle events including generation-1 stale rejection;
- a Git diff guard confirmed no change under `static/` or `tokens.css`.

### Supply-chain review

The broad `rig` facade was rejected after an isolated 644-record resolution. The accepted split/default-off graph produces 194 lockfile records including Meld: 193 external crates, up from 60 in Phase 2, a 133-package expansion. All sources are checksum-locked crates.io packages; there are no Git or alternate-registry sources.

Reqwest/Rustls defaults would have introduced AWS-LC and CMake. Meld pins Reqwest's `rustls-no-provider` and explicitly installs Ring through Rustls, removing AWS-LC from the graph while documenting Ring as an active native build/link boundary. Metadata build targets, proc macros, links, duplicate versions, licenses, and sources are recorded in `SUPPLY_CHAIN_SECURITY.md`.

Pinned `cargo-audit 0.22.2` under `/tmp` loaded 1,226 RustSec advisories and found no vulnerability across the 194-record lockfile in a cached `--no-yanked` scan. Yanked status is not claimed.

### Documentation and files

Added `README.md`, `.env.example`, `src/rig_worker.rs`, `tests/rig_worker.rs`, and `tests/incident_verification.rs`. Updated the manifest/lockfile, domain incident types, deterministic verifier, API mode wiring, binary configuration, existing tests, and all five engineering documents.

`docs/RUST_LEARNING.md` now explains the exact Phase 3 Rust mechanisms: Cargo features, the second object-safe async trait boundary, owned provider futures, Serde/Schemars extraction, proposal-versus-verification, Tokio provider timeout, safe error mapping, process-wide crypto initialization, and validated environment configuration.

### Remaining limitations

- A scoped OpenAI key and network/credit availability are still required for the controlled live smoke.
- The feature-enabled supply chain is much larger than the deterministic fallback.
- Provider rate limits and quota errors are grouped into the safe provider-failure category rather than surfaced with fine-grained retry policy.
- State remains in memory and a process restart loses the mission.
- The application reads environment variables directly; it does not automatically load `.env.local`.

## 2026-08-25 — Phase 3.1: GitHub Actions recovery workflow

Meld now has an actual developer workflow rather than a demo-specific chat adapter. `.github/workflows/meld-recovery.yml` is manually dispatched, builds the requested execution mode, starts the Axum server, and calls it through `scripts/verify-recovery.sh`. The script polls only the public task snapshot and requires Worker B generation 2, one verification pass, one completion, and the generation-1/current-generation-2 stale rejection.

The local deterministic rehearsal passed over HTTP with task 1, Worker B, generation 2, and final event sequence 13. Server logs contained the complete authoritative progression from `task.created` through `submission.stale_rejected`. The workflow publishes those backend events in GitHub's job summary.

The workflow uses read-only permissions, manual dispatch, an exact checkout action commit, disabled checkout credential persistence, a pinned runner/toolchain, and the committed Cargo lockfile. Deterministic mode receives no provider secret. Rig mode evaluates the repository secret only in the server-start step and remains intentionally unrun while the current OpenAI project reports insufficient quota.
