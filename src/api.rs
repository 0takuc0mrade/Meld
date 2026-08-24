//! Axum transport for Meld's authoritative supervisor state.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::domain::{
    AcceptanceCriteria, Assignment, FailureReason, Mission, TaskId, TaskState, TerminalFailure,
    WorkerOutput,
};
use crate::events::{EventKind, MeldEvent};
use crate::supervisor::{Supervisor, SupervisorError, TaskSnapshot};
use crate::worker::{ControlledDelayWorker, SuccessfulWorker, Worker};

const LAST_EVENT_ID: &str = "last-event-id";

#[derive(Clone, Copy, Debug)]
pub struct DemoConfig {
    pub lease_duration: Duration,
    pub first_worker_delay: Duration,
    pub second_worker_delay: Duration,
}

impl Default for DemoConfig {
    fn default() -> Self {
        Self {
            lease_duration: Duration::from_millis(2_400),
            first_worker_delay: Duration::from_millis(4_800),
            second_worker_delay: Duration::from_millis(650),
        }
    }
}

#[derive(Clone)]
pub struct ApiState {
    supervisor: Arc<Supervisor>,
    demo: DemoConfig,
}

impl ApiState {
    pub fn new(supervisor: Arc<Supervisor>) -> Self {
        Self {
            supervisor,
            demo: DemoConfig::default(),
        }
    }

    pub fn with_demo_config(supervisor: Arc<Supervisor>, demo: DemoConfig) -> Self {
        Self { supervisor, demo }
    }

    pub fn supervisor(&self) -> &Arc<Supervisor> {
        &self.supervisor
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/tokens.css", get(tokens_css))
        .route("/styles.css", get(styles_css))
        .route("/app.js", get(app_js))
        .route("/api/health", get(health))
        .route("/api/missions/demo", post(start_demo))
        .route("/api/tasks/{task_id}", get(task_snapshot))
        .route("/api/tasks/{task_id}/events", get(task_events))
        .fallback(not_found)
        .with_state(state)
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    version: &'static str,
    phase: u8,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ready",
        service: "meld",
        version: env!("CARGO_PKG_VERSION"),
        phase: 2,
    })
}

#[derive(Debug, Serialize)]
struct StartDemoResponse {
    task_id: u64,
    snapshot: TaskSnapshotResponse,
}

async fn start_demo(
    State(state): State<ApiState>,
) -> Result<(StatusCode, Json<StartDemoResponse>), ApiError> {
    let mission = Mission {
        title: "Recover a reliability brief".to_owned(),
        objective: "Explain how assignment generations keep late agent work from replacing the verified result."
            .to_owned(),
        acceptance: AcceptanceCriteria {
            minimum_summary_chars: 40,
            required_terms: vec!["generation".to_owned(), "stale".to_owned()],
            minimum_evidence_items: 1,
        },
    };
    let task_id = state
        .supervisor
        .create_task(mission)
        .await
        .map_err(ApiError::from_supervisor)?;
    let snapshot = state
        .supervisor
        .snapshot(task_id)
        .await
        .map(TaskSnapshotResponse::from)
        .map_err(ApiError::from_supervisor)?;

    let supervisor = Arc::clone(&state.supervisor);
    let config = state.demo;
    tokio::spawn(async move {
        let workers: Vec<Arc<dyn Worker>> = vec![
            Arc::new(ControlledDelayWorker::new(
                SuccessfulWorker::new("Worker A", WorkerOutput::accepted_fixture("Worker A")),
                config.first_worker_delay,
            )),
            Arc::new(ControlledDelayWorker::new(
                SuccessfulWorker::new("Worker B", WorkerOutput::accepted_fixture("Worker B")),
                config.second_worker_delay,
            )),
        ];

        if let Err(error) = supervisor
            .run_task(task_id, workers, config.lease_duration)
            .await
        {
            tracing::error!(task_id = task_id.0, %error, "demo mission supervision failed");
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(StartDemoResponse {
            task_id: task_id.0,
            snapshot,
        }),
    ))
}

async fn task_snapshot(
    State(state): State<ApiState>,
    Path(raw_task_id): Path<String>,
) -> Result<Json<TaskSnapshotResponse>, ApiError> {
    let task_id = parse_task_id(&raw_task_id)?;
    state
        .supervisor
        .snapshot(task_id)
        .await
        .map(TaskSnapshotResponse::from)
        .map(Json)
        .map_err(ApiError::from_supervisor)
}

async fn task_events(
    State(state): State<ApiState>,
    Path(raw_task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let task_id = parse_task_id(&raw_task_id)?;
    let cursor = parse_last_event_id(&headers)?;

    // Subscribe before reading history. Events authored during the snapshot read
    // are present in both sources and are removed by the sequence cursor below.
    let mut receiver = state.supervisor.state().subscribe();
    let snapshot = state
        .supervisor
        .snapshot(task_id)
        .await
        .map_err(ApiError::from_supervisor)?;
    let replay = snapshot.events;
    let (sender, stream_receiver) = mpsc::channel(32);

    tokio::spawn(async move {
        let mut last_sequence = cursor;
        for event in replay {
            if event.sequence > last_sequence {
                last_sequence = event.sequence;
                if sender.send(sse_domain_event(event)).await.is_err() {
                    return;
                }
            }
        }

        loop {
            match receiver.recv().await {
                Ok(event) if event.task_id == task_id && event.sequence > last_sequence => {
                    last_sequence = event.sequence;
                    if sender.send(sse_domain_event(event)).await.is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    if sender
                        .send(sse_resync_event(task_id, skipped))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    let stream = ReceiverStream::new(stream_receiver).map(Ok::<Event, Infallible>);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("meld-keepalive"),
    ))
}

fn sse_domain_event(event: MeldEvent) -> Event {
    let id = event.sequence.to_string();
    Event::default()
        .id(id)
        .event("meld")
        .json_data(EventResponse::from(event))
        .expect("event response contains only serializable primitives")
}

#[derive(Serialize)]
struct ResyncResponse {
    task_id: u64,
    reason: &'static str,
    skipped_events: u64,
}

fn sse_resync_event(task_id: TaskId, skipped_events: u64) -> Event {
    Event::default()
        .event("resync")
        .json_data(ResyncResponse {
            task_id: task_id.0,
            reason: "subscriber_lagged",
            skipped_events,
        })
        .expect("resync response contains only serializable primitives")
}

fn parse_task_id(raw: &str) -> Result<TaskId, ApiError> {
    raw.parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .map(TaskId)
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_task_id",
                "Task ID must be a positive whole number.",
            )
        })
}

fn parse_last_event_id(headers: &HeaderMap) -> Result<u64, ApiError> {
    let Some(value) = headers.get(LAST_EVENT_ID) else {
        return Ok(0);
    };
    let value = value.to_str().map_err(|_| {
        ApiError::bad_request(
            "invalid_event_cursor",
            "Last-Event-ID must be a valid event sequence number.",
        )
    })?;
    value.parse::<u64>().map_err(|_| {
        ApiError::bad_request(
            "invalid_event_cursor",
            "Last-Event-ID must be a valid event sequence number.",
        )
    })
}

#[derive(Debug, Serialize)]
pub struct TaskSnapshotResponse {
    task_id: u64,
    mission: MissionResponse,
    status: TaskStatusResponse,
    current_sequence: u64,
    accepted_result: Option<AcceptedResultResponse>,
    failure: Option<String>,
    events: Vec<EventResponse>,
}

impl From<TaskSnapshot> for TaskSnapshotResponse {
    fn from(snapshot: TaskSnapshot) -> Self {
        let current_sequence = snapshot.events.last().map_or(0, |event| event.sequence);
        let (status, accepted_result, failure) = task_state_response(&snapshot.state);
        Self {
            task_id: snapshot.id.0,
            mission: MissionResponse::from(&snapshot.mission),
            status,
            current_sequence,
            accepted_result,
            failure,
            events: snapshot
                .events
                .into_iter()
                .map(EventResponse::from)
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct MissionResponse {
    title: String,
    objective: String,
    acceptance_policy: AcceptancePolicyResponse,
}

impl From<&Mission> for MissionResponse {
    fn from(mission: &Mission) -> Self {
        Self {
            title: mission.title.clone(),
            objective: mission.objective.clone(),
            acceptance_policy: AcceptancePolicyResponse {
                minimum_summary_chars: mission.acceptance.minimum_summary_chars,
                required_terms: mission.acceptance.required_terms.clone(),
                minimum_evidence_items: mission.acceptance.minimum_evidence_items,
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct AcceptancePolicyResponse {
    minimum_summary_chars: usize,
    required_terms: Vec<String>,
    minimum_evidence_items: usize,
}

#[derive(Debug, Serialize)]
struct TaskStatusResponse {
    name: &'static str,
    label: &'static str,
    worker_id: Option<String>,
    assignment_id: Option<u64>,
    generation: Option<u32>,
    next_generation: Option<u32>,
    lease_remaining_ms: Option<u64>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct AcceptedResultResponse {
    worker_id: String,
    assignment_id: u64,
    submission_id: u64,
    generation: u32,
    summary: String,
    evidence: Vec<String>,
    verified_at_ms: u64,
}

fn task_state_response(
    state: &TaskState,
) -> (
    TaskStatusResponse,
    Option<AcceptedResultResponse>,
    Option<String>,
) {
    match state {
        TaskState::Pending => (status("pending", "Mission queued"), None, None),
        TaskState::Assigned { assignment } => (
            assignment_status("assigned", "Worker assigned", assignment),
            None,
            None,
        ),
        TaskState::Running { assignment, .. } => (
            assignment_status("running", "Worker executing", assignment),
            None,
            None,
        ),
        TaskState::Recovering {
            expired,
            reason,
            next_generation,
        } => (
            TaskStatusResponse {
                name: "recovering",
                label: "Failure detected — reassigning",
                worker_id: Some(expired.worker_id.to_string()),
                assignment_id: Some(expired.id.0),
                generation: Some(expired.generation.0),
                next_generation: Some(next_generation.0),
                lease_remaining_ms: None,
                reason: Some(reason.to_string()),
            },
            None,
            None,
        ),
        TaskState::Verifying {
            assignment,
            submission: _,
        } => (
            assignment_status("verifying", "Checking acceptance policy", assignment),
            None,
            None,
        ),
        TaskState::Completed { accepted, .. } => (
            status("completed", "Mission completed"),
            Some(AcceptedResultResponse {
                worker_id: accepted.submission.worker_id.to_string(),
                assignment_id: accepted.submission.token.assignment_id.0,
                submission_id: accepted.submission.id.0,
                generation: accepted.submission.token.generation.0,
                summary: accepted.submission.output.summary.clone(),
                evidence: accepted.submission.output.evidence.clone(),
                verified_at_ms: system_time_ms(accepted.verified_at),
            }),
            None,
        ),
        TaskState::Failed { reason, .. } => {
            let failure = terminal_failure_message(reason);
            (
                TaskStatusResponse {
                    name: "failed",
                    label: "Mission failed",
                    reason: Some(failure.clone()),
                    ..status("failed", "Mission failed")
                },
                None,
                Some(failure),
            )
        }
    }
}

fn status(name: &'static str, label: &'static str) -> TaskStatusResponse {
    TaskStatusResponse {
        name,
        label,
        worker_id: None,
        assignment_id: None,
        generation: None,
        next_generation: None,
        lease_remaining_ms: None,
        reason: None,
    }
}

fn assignment_status(
    name: &'static str,
    label: &'static str,
    assignment: &Assignment,
) -> TaskStatusResponse {
    TaskStatusResponse {
        name,
        label,
        worker_id: Some(assignment.worker_id.to_string()),
        assignment_id: Some(assignment.id.0),
        generation: Some(assignment.generation.0),
        next_generation: None,
        lease_remaining_ms: Some(
            assignment
                .deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        ),
        reason: None,
    }
}

fn terminal_failure_message(reason: &TerminalFailure) -> String {
    match reason {
        TerminalFailure::WorkersExhausted { last_reason } => last_reason.as_ref().map_or_else(
            || "No workers were available to execute this mission.".to_owned(),
            |reason| format!("All workers were exhausted after {reason}."),
        ),
    }
}

#[derive(Clone, Debug, Serialize)]
struct EventResponse {
    sequence: u64,
    task_id: u64,
    occurred_at_ms: u64,
    kind: &'static str,
    message: String,
    worker_id: Option<String>,
    from_worker_id: Option<String>,
    to_worker_id: Option<String>,
    assignment_id: Option<u64>,
    submission_id: Option<u64>,
    generation: Option<u32>,
    submitted_generation: Option<u32>,
    current_generation: Option<u32>,
    reason: Option<String>,
}

impl From<MeldEvent> for EventResponse {
    fn from(event: MeldEvent) -> Self {
        let mut response = Self {
            sequence: event.sequence,
            task_id: event.task_id.0,
            occurred_at_ms: system_time_ms(event.occurred_at),
            kind: event.kind.name(),
            message: String::new(),
            worker_id: None,
            from_worker_id: None,
            to_worker_id: None,
            assignment_id: None,
            submission_id: None,
            generation: None,
            submitted_generation: None,
            current_generation: None,
            reason: None,
        };

        match event.kind {
            EventKind::TaskCreated => response.message = "Mission created".to_owned(),
            EventKind::TaskAssigned {
                worker_id,
                assignment_id,
                generation,
            } => {
                response.message = format!("Assigned generation {generation} to {worker_id}");
                response.worker_id = Some(worker_id.to_string());
                response.assignment_id = Some(assignment_id.0);
                response.generation = Some(generation.0);
            }
            EventKind::WorkerStarted {
                worker_id,
                assignment_id,
                generation,
            } => {
                response.message = format!("{worker_id} started generation {generation}");
                response.worker_id = Some(worker_id.to_string());
                response.assignment_id = Some(assignment_id.0);
                response.generation = Some(generation.0);
            }
            EventKind::WorkerFailed {
                worker_id,
                assignment_id,
                generation,
                reason,
            } => {
                response.message = match reason {
                    FailureReason::DeadlineExceeded => {
                        format!("{worker_id} exceeded its backend lease")
                    }
                    _ => format!("{worker_id} could not finish the assignment"),
                };
                response.worker_id = Some(worker_id.to_string());
                response.assignment_id = Some(assignment_id.0);
                response.generation = Some(generation.0);
                response.reason = Some(reason.to_string());
            }
            EventKind::AssignmentExpired {
                worker_id,
                assignment_id,
                generation,
            } => {
                response.message = format!("Generation {generation} lease expired");
                response.worker_id = Some(worker_id.to_string());
                response.assignment_id = Some(assignment_id.0);
                response.generation = Some(generation.0);
            }
            EventKind::TaskReassigned {
                from,
                to,
                assignment_id,
                generation,
            } => {
                response.message = format!("Meld reassigned the mission from {from} to {to}");
                response.from_worker_id = Some(from.to_string());
                response.to_worker_id = Some(to.to_string());
                response.assignment_id = Some(assignment_id.0);
                response.generation = Some(generation.0);
            }
            EventKind::SubmissionReceived {
                worker_id,
                submission_id,
                assignment_id,
                generation,
            } => {
                response.message = format!("Received {worker_id}’s generation {generation} result");
                response.worker_id = Some(worker_id.to_string());
                response.submission_id = Some(submission_id.0);
                response.assignment_id = Some(assignment_id.0);
                response.generation = Some(generation.0);
            }
            EventKind::SubmissionRejected {
                worker_id,
                assignment_id,
                generation,
                reason,
            } => {
                response.message =
                    format!("Rejected {worker_id}’s result — it was not authoritative");
                response.worker_id = Some(worker_id.to_string());
                response.assignment_id = Some(assignment_id.0);
                response.generation = Some(generation.0);
                response.reason = Some(reason.to_string());
            }
            EventKind::StaleSubmissionRejected {
                worker_id,
                assignment_id,
                submitted,
                current,
            } => {
                response.message =
                    "Old result rejected — this mission had already been reassigned".to_owned();
                response.worker_id = Some(worker_id.to_string());
                response.assignment_id = Some(assignment_id.0);
                response.submitted_generation = Some(submitted.0);
                response.current_generation = Some(current.0);
                response.reason = Some(format!(
                    "Generation {submitted} was stale; generation {current} remained authoritative."
                ));
            }
            EventKind::VerificationStarted { submission_id } => {
                response.message = "Meld started deterministic acceptance checks".to_owned();
                response.submission_id = Some(submission_id.0);
            }
            EventKind::VerificationFailed {
                submission_id,
                code,
            } => {
                response.message = "The result did not satisfy the acceptance policy".to_owned();
                response.submission_id = Some(submission_id.0);
                response.reason = Some(code.to_string());
            }
            EventKind::VerificationPassed { submission_id } => {
                response.message =
                    "Output satisfied Meld’s deterministic acceptance policy".to_owned();
                response.submission_id = Some(submission_id.0);
            }
            EventKind::TaskCompleted {
                submission_id,
                generation,
            } => {
                response.message = format!("Mission completed with generation {generation}");
                response.submission_id = Some(submission_id.0);
                response.generation = Some(generation.0);
            }
            EventKind::TaskFailed { reason } => {
                response.message = "Mission ended after all workers were exhausted".to_owned();
                response.reason = Some(terminal_failure_message(&reason));
            }
        }

        response
    }
}

fn system_time_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    fn bad_request(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message,
        }
    }

    fn from_supervisor(error: SupervisorError) -> Self {
        match error {
            SupervisorError::TaskNotFound(_) => Self {
                status: StatusCode::NOT_FOUND,
                code: "task_not_found",
                message: "That task does not exist.",
            },
            other => {
                tracing::error!(error = %other, "API request failed inside the supervisor");
                Self {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    code: "internal_error",
                    message: "Meld could not complete the request.",
                }
            }
        }
    }
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorResponse,
}

#[derive(Serialize)]
struct ErrorResponse {
    code: &'static str,
    message: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorResponse {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

async fn index() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; base-uri 'none'; form-action 'self'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Html(include_str!("../static/index.html")),
    )
}

async fn tokens_css() -> impl IntoResponse {
    static_asset("text/css; charset=utf-8", include_str!("../tokens.css"))
}

async fn styles_css() -> impl IntoResponse {
    static_asset(
        "text/css; charset=utf-8",
        include_str!("../static/styles.css"),
    )
}

async fn app_js() -> impl IntoResponse {
    static_asset(
        "text/javascript; charset=utf-8",
        include_str!("../static/app.js"),
    )
}

fn static_asset(content_type: &'static str, body: &'static str) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
}

async fn not_found() -> ApiError {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "route_not_found",
        message: "That Meld route does not exist.",
    }
}
