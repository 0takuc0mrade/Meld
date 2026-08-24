# Learning Rust Through Meld

This document explains Rust and systems concepts only where they appear in Meld. Code snippets are simplified and may omit domain details.

## Enums as the task state machine

**Where:** `TaskState`, `EventKind`, `FailureReason`, and typed rejection errors.

**Why:** A task must be in exactly one authoritative state. An enum packages the data required by that state, such as the active assignment in `Running`.

**Problem solved:** A design based on booleans such as `is_running`, `is_failed`, and `is_complete` can represent contradictions. Rust enums make states mutually exclusive and force `match` expressions to consider every variant.

**Without it:** `completed = true` and `running = true` could coexist, or a running task could have no assignment.

```rust
match task.state {
    TaskState::Running { ref assignment, .. } => expire(assignment),
    TaskState::Completed { .. } => return Err(AlreadyTerminal),
    _ => return Err(InvalidTransition),
}
```

The compiler checks that the match remains exhaustive when a new state is added.

## Newtypes

**Where:** `TaskId`, `AssignmentId`, `Generation`, and `SubmissionId`.

**Why:** All may be represented by integers, but they mean different things.

**Problem solved:** `submit(task_id, assignment_id)` cannot silently accept them in the wrong order if each has a distinct type.

**Without it:** Two `u64` values compile even when swapped.

```rust
struct TaskId(u64);
struct AssignmentId(u64);
```

Newtypes have no runtime cost after optimization; they add compile-time meaning.

## Ownership in worker requests

**Where:** A `WorkRequest` is moved into a spawned worker task. Shared application state is cloned as an `Arc`, not deeply copied.

**Why:** A spawned future may outlive the function that created it, so it cannot borrow short-lived stack data.

**Problem solved:** Moving owned request data into the task guarantees it remains valid for the task's entire lifetime.

**Without it:** Rust rejects a task that captures a non-`'static` reference which may disappear.

```rust
let app = Arc::clone(&app);
tokio::spawn(async move {
    let output = worker.execute(request).await;
    app.supervisor.submit(token, output).await;
});
```

`move` transfers captured values into the async block. Cloning an `Arc` clones a reference count, not the underlying application state.

## `Arc`

**Where:** The Axum router, supervisor tasks, deadline tasks, and workers need access to shared services.

**Why:** `Arc<T>` provides thread-safe shared ownership. Tokio may move spawned tasks between runtime threads.

**Problem solved:** There can be multiple owners without choosing one artificial lifetime owner.

**Without it:** A borrowed `&AppState` generally cannot be moved into an independently running spawned task, while moving the one `AppState` would make it unavailable elsewhere.

**Concurrency implication:** `Arc` only protects the reference count. Interior mutation still needs synchronization, so the store is `Arc<Mutex<RuntimeState>>`.

## `tokio::sync::Mutex`

**Where:** The in-memory task map, active assignments, retry counters, and event history.

**Why:** Worker completion and deadline expiration can happen concurrently. Checking whether a token is current and changing state must be atomic.

**Problem solved:** Only one state transition can inspect and mutate the store at a time.

**Without it:** A deadline and successful submission could both believe they won and publish contradictory outcomes.

```rust
let events = {
    let mut store = self.store.lock().await;
    store.apply(command)?
}; // guard is dropped here

self.publish(events);
```

The important pattern is to release the guard before publishing or awaiting external work. Holding an async mutex across an LLM call would block all other transitions and can create deadlocks or long stalls.

## Why not `RwLock` initially?

`RwLock` allows many readers or one writer. Meld's important operations usually read current state and then mutate it atomically. The task set is tiny, and the UI mostly reads an event stream. A mutex is easier to reason about. If profiling later shows snapshot reads cause contention, the decision can be revisited.

## `async` and `.await`

**Where:** HTTP handlers, worker execution, mutex acquisition, SSE, and shutdown.

**Why:** These operations wait for I/O or time without blocking an operating-system thread.

**Problem solved:** Meld can keep serving snapshots and processing deadlines while a worker waits on a model provider.

**Without it:** A blocking worker request could occupy a server thread for its entire duration.

Calling `.await` is a suspension point. Local state captured across it must remain valid, and a spawned future generally must be `Send`.

## Tokio tasks and `tokio::spawn`

**Where:** One task executes each assignment and another observes its lease deadline.

**Why:** The result and deadline are independent concurrent events.

**Problem solved:** Meld can reassign after the deadline while Worker A continues and later demonstrates stale-result rejection.

**Without it:** Awaiting Worker A directly would prevent the supervisor from noticing its own deadline until the worker returned.

`tokio::spawn` returns a `JoinHandle`. Awaiting it yields either the task result or a `JoinError`, which can reveal cancellation or panic. Meld translates a panic into a typed worker failure; it does not let a worker panic become authoritative state.

## Monotonic time and deadlines

**Where:** `Assignment.issued_at`, `Assignment.deadline`, and deadline tasks.

**Why:** Durations must not be affected by wall-clock corrections.

**Problem solved:** `tokio::time::Instant` is monotonic, so a clock change cannot extend or shorten a lease unexpectedly.

**Without it:** Using `SystemTime` for timeout arithmetic can behave strangely when the system clock jumps.

`SystemTime` is still appropriate for human-readable event timestamps. The two clocks serve different purposes.

Tokio time can be paused in tests, allowing a test to advance directly past a deadline without sleeping in real time.

## Traits

**Where:** `Worker` and `Verifier`.

**Why:** The supervisor needs behavior, not knowledge of whether a worker is Rig-backed, deliberately late, or deterministic.

**Problem solved:** Tests can substitute controlled implementations while exercising the exact production supervisor.

**Without it:** Failure scenarios would require network calls or special-case branches inside the state machine.

For heterogeneous workers, the initial design returns a boxed future:

```rust
type WorkerFuture =
    Pin<Box<dyn Future<Output = Result<WorkerOutput, WorkerError>> + Send + 'static>>;

trait Worker {
    fn execute(&self, request: WorkRequest) -> WorkerFuture;
}
```

The `Pin<Box<...>>` says the async computation lives at a stable heap address and can be called through a trait object. This avoids an `async-trait` dependency, at the cost of a more advanced signature. The tradeoff is recorded in `DECISIONS.md`.

## `Result` and `thiserror`

**Where:** Every command that can be rejected, worker execution, verification, and API translation.

**Why:** Expected failure is data, not a crash.

**Problem solved:** Callers must explicitly handle success and error variants. `thiserror` derives readable error implementations without losing typed variants.

```rust
#[derive(Debug, thiserror::Error)]
enum SubmitError {
    #[error("assignment generation {submitted:?} is stale; current is {current:?}")]
    Stale { submitted: Generation, current: Generation },
}
```

Worker errors, invalid commands, and internal bugs should not all become the same HTTP 500 response. They have different recovery and observability meanings.

## Serde at the transport boundary

**Where:** Response DTOs in `src/api.rs`.

**Why:** The Rust domain needs a controlled boundary to browser JSON.

**Problem solved:** Derives keep serialization consistent with typed Rust structures.

**Without it:** Manual parsing would be verbose and more likely to accept malformed or ambiguous payloads.

Deferring Serde kept Phase 1's dependency graph smaller and prevented transport annotations from shaping the domain. Phase 2 derives `Serialize` only on API response types. Domain types still have no Serde annotations, and the demo command accepts no browser-authored mission payload, so deserialization cannot bypass domain policy.

## Broadcast channels

**Where:** The supervisor publishes committed domain events to SSE connections through `tokio::sync::broadcast`.

**Why:** Several browser tabs or internal observers may want the same event.

**Problem solved:** The supervisor does not need to know or await each subscriber.

**Without it:** HTTP handlers would need to poll or the supervisor would need a custom subscriber registry.

Broadcast delivery is best effort. A slow subscriber can lag, which is why task snapshots and bounded event history remain authoritative.

## `Send`, `Sync`, and `'static`

**Where:** `Worker: Send + Sync`, `Verifier: Send + Sync`, `WorkerFuture + Send + 'static`, and every closure passed to `tokio::spawn`.

**Why:** Tokio's multi-thread runtime may move a task between worker threads. A spawned task may also outlive the stack frame that created it.

**Problem solved:** `Send` permits ownership to move between threads, `Sync` permits shared references from multiple threads, and `'static` ensures the future contains no borrowed reference that can expire while the task is still alive.

**Without it:** Rust refuses to spawn the future, which prevents a use-after-free rather than discovering it at runtime.

`'static` does not mean “lives forever.” It means the future owns its captured data (or refers only to truly static data). `run_task` clones `Arc<Supervisor>`, `WorkerId`, `Mission`, and tokens into `async move` blocks for this reason.

## `tokio::select!` and the deadline race

**Where:** `Supervisor::run_task` waits on the worker `JoinHandle` and deadline `JoinHandle`.

**Why:** Completion and expiry are concurrent facts; neither ordering is inherently an error.

**Problem solved:** Meld responds promptly to the first outcome without pretending the other side ceased to exist.

```rust
tokio::select! {
    joined = &mut worker_task => handle_worker(joined),
    expired = &mut deadline_task => handle_deadline(expired),
}
```

If both become ready together, Tokio may choose either branch. Safety does not depend on which it chooses: the worker submission and deadline expiry each reacquire the same store mutex and compare the same assignment token. The first valid transition changes the state; the second observes that its precondition no longer holds.

The exact-deadline integration test intentionally accepts either generation 1 or generation 2, but requires one and only one completion event and a valid completed state.

## Dropping a `JoinHandle` is not cancellation

**Where:** After `tokio::select!`, the losing handle leaves scope.

**Why:** Meld wants the old worker to return later and prove stale-result rejection. It also wants a late deadline to prove completed-state protection.

**Problem solved:** Tokio detaches a task when its `JoinHandle` is dropped; the task continues running. `abort()` would request cancellation instead.

**Concurrency implication:** Detached tasks need a separate lifecycle strategy at server shutdown. Phase 2 should track them for draining or bounded cancellation. Even then, token checks remain the correctness mechanism because remote workers cannot be reliably aborted.

## Atomic check-and-transition

**Where:** `expire_assignment`, `record_worker_failure`, `submit_result`, and `record_verification`.

**Why:** Checking a token and changing state in separate lock acquisitions would create a time-of-check/time-of-use gap.

**Problem solved:** Each method performs validation and its one state mutation while holding one mutex guard. Competing tasks cannot both validate the same old state.

**Without it:** The deadline could validate `Running`, the worker could complete, and then the deadline could overwrite `Completed` with `Recovering`.

Event sequence allocation and history insertion occur inside the same critical section as the transition. Tracing and broadcast publication happen afterward so observers only see committed facts without extending lock duration.

## Tracing and spans

**Where:** Task creation, assignment, deadline, failure, submission, verification, and completion.

**Why:** Concurrent logs are difficult to follow without structured identifiers.

**Problem solved:** A task span attaches fields such as `task_id`, `assignment_id`, `generation`, and `worker_id` to related records.

```rust
tracing::info!(
    task_id = %task_id,
    generation = generation.0,
    event = "assignment.expired",
    "assignment deadline elapsed"
);
```

Prompts, secrets, and complete model outputs are not tracing fields. Structured logging makes redaction policy more important, not less.

## Cancellation and late work

**Where:** A worker may continue after its assignment expires.

**Why:** Remote workers cannot always be forcibly canceled, even if an in-process future can be dropped.

**Problem solved:** Meld relies on generation checks for safety, not successful cancellation. Cancellation is an efficiency feature; stale-result rejection is the correctness feature.

For the demo, Worker A intentionally remains alive long enough to submit generation 1 after generation 2 completes. Later, a cancellation grace period can abort local tasks to conserve resources.

## Graceful shutdown

**Where:** Axum server termination and background task coordination.

**Why:** The server should stop accepting new work and report what happens to in-flight tasks.

**Problem solved:** Abrupt termination can truncate logs and leave clients uncertain.

The in-memory MVP cannot resume tasks after process death. Graceful shutdown improves behavior but does not create durability; that requires persistent state and recovery logic later.

## Axum `Router` and handlers

**Where:** `api::router`, `health`, `start_demo`, `task_snapshot`, and `task_events`.

**What:** A `Router` matches an HTTP method and path to an async Rust function. Axum turns a handler’s extractors into inputs and anything implementing `IntoResponse` into the HTTP response.

**Why Meld needs it:** The browser needs a narrow way to create the controlled mission, read authority, and observe committed events. Handlers call `Supervisor`; they do not duplicate transition code.

**Connection to existing concepts:** A handler is an async function running on the same Tokio runtime as worker/deadline tasks. Axum can run many handlers concurrently, while `Arc<Supervisor>` and its mutex preserve the single authority.

## Axum extractors: `State` and `Path`

**Where:** `State<ApiState>` shares the supervisor; `Path<String>` obtains a task ID from the URL.

**What:** Extractors ask Axum to construct typed handler inputs from application state or the request.

**Why the task ID starts as `String`:** Meld parses it itself so malformed values receive the same typed JSON error envelope as other API failures instead of Axum’s default text rejection.

**Ownership connection:** `ApiState` is cheap to clone because it contains an `Arc<Supervisor>`. Each handler owns its clone while all clones point to the same store.

## JSON responses and typed HTTP errors

**Where:** `Json<T>`, `TaskSnapshotResponse`, `EventResponse`, and `ApiError::into_response`.

**What:** Serde converts response structs containing strings/numbers/options into JSON. `ApiError` pairs an HTTP status with a stable code and safe message.

**Problem solved:** Browsers can distinguish `invalid_task_id`, `task_not_found`, and `internal_error`; internal Rust debug output never crosses the boundary. Detailed context remains in tracing.

## SSE as an async stream

**Where:** `task_events` and its `ReceiverStream`.

**What:** The handler returns a stream of `Event` values instead of one completed body. Axum polls it whenever the socket can accept more bytes; the task does not block an OS thread while waiting.

**Why Meld needs it:** Domain events travel server-to-browser immediately, and native `EventSource` reconnects with the last SSE ID. HTTP remains responsible for commands and snapshots.

**Ownership/lifetime connection:** The spawned forwarding task owns its broadcast receiver, replay vector, cursor, task ID, and MPSC sender. Owning those values satisfies the `'static` requirement of `tokio::spawn`.

## Broadcast lag and the MPSC bridge

**Where:** `task_events` receives from `broadcast`, then sends framed SSE events through a 32-item `mpsc` channel wrapped by `ReceiverStream`.

**What:** Broadcast fans each committed fact to all subscribers. MPSC gives one HTTP response a bounded producer/consumer bridge.

**Problem solved:** If broadcast returns `Lagged`, the handler does not pretend delivery was complete. It emits `event: resync`; JavaScript refetches the snapshot and bounded history. Browser speed never backpressures the supervisor.

## Graceful server shutdown

**Where:** `main` awaits `tokio::signal::ctrl_c()` through Axum’s `with_graceful_shutdown`.

**What:** The listener stops accepting new connections and lets active HTTP work wind down.

**Limit:** Detached controlled workers are still protected by generation checks, but Phase 2 does not yet maintain a global `JoinSet` to drain them. Process exit still loses the in-memory store.
