use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::{Mutex, broadcast};
use tokio::task::JoinError;
use tokio::time::{Instant, sleep_until};

use crate::domain::{
    Assignment, AssignmentId, AssignmentToken, FailureReason, Generation, Mission, Submission,
    SubmissionId, SubmissionRejection, TaskId, TaskState, TerminalFailure, VerificationError,
    VerifiedOutput, WorkRequest, WorkerError, WorkerId, WorkerOutput,
};
use crate::events::{EventKind, MeldEvent};
use crate::verifier::Verifier;
use crate::worker::Worker;

const DEFAULT_EVENT_HISTORY_LIMIT: usize = 256;
const DEFAULT_EVENT_CHANNEL_CAPACITY: usize = 256;

#[derive(Clone, Debug)]
pub struct TaskSnapshot {
    pub id: TaskId,
    pub mission: Mission,
    pub state: TaskState,
    pub last_generation: Generation,
    pub events: Vec<MeldEvent>,
}

#[derive(Debug)]
struct TaskRecord {
    id: TaskId,
    mission: Mission,
    state: TaskState,
    last_generation: Generation,
    events: VecDeque<MeldEvent>,
}

#[derive(Debug)]
struct RuntimeState {
    tasks: HashMap<TaskId, TaskRecord>,
    next_task_id: u64,
    next_assignment_id: u64,
    next_submission_id: u64,
    next_event_sequence: u64,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            tasks: HashMap::new(),
            next_task_id: 1,
            next_assignment_id: 1,
            next_submission_id: 1,
            next_event_sequence: 1,
        }
    }
}

impl RuntimeState {
    fn append_event(
        &mut self,
        task_id: TaskId,
        kind: EventKind,
        history_limit: usize,
    ) -> Result<MeldEvent, SupervisorError> {
        let event = MeldEvent {
            sequence: self.next_event_sequence,
            task_id,
            occurred_at: SystemTime::now(),
            kind,
        };
        self.next_event_sequence = self
            .next_event_sequence
            .checked_add(1)
            .expect("event sequence overflow");

        let task = self
            .tasks
            .get_mut(&task_id)
            .ok_or(SupervisorError::TaskNotFound(task_id))?;
        while task.events.len() >= history_limit {
            task.events.pop_front();
        }
        task.events.push_back(event.clone());
        Ok(event)
    }
}

pub struct AppState {
    store: Mutex<RuntimeState>,
    event_sender: broadcast::Sender<MeldEvent>,
    event_history_limit: usize,
}

impl AppState {
    pub fn new(event_history_limit: usize, event_channel_capacity: usize) -> Self {
        let (event_sender, _) = broadcast::channel(event_channel_capacity.max(1));
        Self {
            store: Mutex::new(RuntimeState::default()),
            event_sender,
            event_history_limit: event_history_limit.max(1),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MeldEvent> {
        self.event_sender.subscribe()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_HISTORY_LIMIT, DEFAULT_EVENT_CHANNEL_CAPACITY)
    }
}

#[derive(Clone)]
pub struct Supervisor {
    state: Arc<AppState>,
    verifier: Arc<dyn Verifier>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionOutcome {
    Applied,
    Ignored,
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("task {0} was not found")]
    TaskNotFound(TaskId),
    #[error("cannot {action} while task {task_id} is {state}")]
    InvalidTransition {
        task_id: TaskId,
        action: &'static str,
        state: &'static str,
    },
    #[error(transparent)]
    SubmissionRejected(#[from] SubmissionRejection),
    #[error("background {role} task failed: {message}")]
    BackgroundTask { role: &'static str, message: String },
}

impl Supervisor {
    pub fn new(state: Arc<AppState>, verifier: Arc<dyn Verifier>) -> Self {
        Self { state, verifier }
    }

    pub fn state(&self) -> &Arc<AppState> {
        &self.state
    }

    pub async fn create_task(&self, mission: Mission) -> Result<TaskId, SupervisorError> {
        let (task_id, events) = {
            let mut store = self.state.store.lock().await;
            let task_id = TaskId(store.next_task_id);
            store.next_task_id = store.next_task_id.checked_add(1).expect("task ID overflow");
            store.tasks.insert(
                task_id,
                TaskRecord {
                    id: task_id,
                    mission,
                    state: TaskState::Pending,
                    last_generation: Generation::default(),
                    events: VecDeque::new(),
                },
            );
            let event = store.append_event(
                task_id,
                EventKind::TaskCreated,
                self.state.event_history_limit,
            )?;
            (task_id, vec![event])
        };
        self.publish(events);
        Ok(task_id)
    }

    pub async fn snapshot(&self, task_id: TaskId) -> Result<TaskSnapshot, SupervisorError> {
        let store = self.state.store.lock().await;
        let task = store
            .tasks
            .get(&task_id)
            .ok_or(SupervisorError::TaskNotFound(task_id))?;
        Ok(TaskSnapshot {
            id: task.id,
            mission: task.mission.clone(),
            state: task.state.clone(),
            last_generation: task.last_generation,
            events: task.events.iter().cloned().collect(),
        })
    }

    pub async fn assign_next_worker(
        &self,
        task_id: TaskId,
        worker_id: WorkerId,
        lease_duration: Duration,
    ) -> Result<Assignment, SupervisorError> {
        let (assignment, events) = {
            let mut store = self.state.store.lock().await;
            let (previous_worker, generation) = {
                let task = store
                    .tasks
                    .get(&task_id)
                    .ok_or(SupervisorError::TaskNotFound(task_id))?;
                let previous_worker = match &task.state {
                    TaskState::Pending => None,
                    TaskState::Recovering { expired, .. } => Some(expired.worker_id.clone()),
                    state => {
                        return Err(SupervisorError::InvalidTransition {
                            task_id,
                            action: "assign worker",
                            state: state.name(),
                        });
                    }
                };
                (previous_worker, task.last_generation.next())
            };

            let assignment_id = AssignmentId(store.next_assignment_id);
            store.next_assignment_id = store
                .next_assignment_id
                .checked_add(1)
                .expect("assignment ID overflow");
            let issued_at = Instant::now();
            let assignment = Assignment {
                id: assignment_id,
                task_id,
                worker_id: worker_id.clone(),
                generation,
                issued_at,
                deadline: issued_at + lease_duration,
            };

            let task = store
                .tasks
                .get_mut(&task_id)
                .ok_or(SupervisorError::TaskNotFound(task_id))?;
            task.last_generation = generation;
            task.state = TaskState::Assigned {
                assignment: assignment.clone(),
            };

            let mut events = Vec::with_capacity(2);
            if let Some(from) = previous_worker {
                events.push(store.append_event(
                    task_id,
                    EventKind::TaskReassigned {
                        from,
                        to: worker_id.clone(),
                        assignment_id,
                        generation,
                    },
                    self.state.event_history_limit,
                )?);
            }
            events.push(store.append_event(
                task_id,
                EventKind::TaskAssigned {
                    worker_id,
                    assignment_id,
                    generation,
                },
                self.state.event_history_limit,
            )?);
            (assignment, events)
        };
        self.publish(events);
        Ok(assignment)
    }

    pub async fn mark_worker_started(&self, token: AssignmentToken) -> Result<(), SupervisorError> {
        let events = {
            let mut store = self.state.store.lock().await;
            let assignment = {
                let task = store
                    .tasks
                    .get(&token.task_id)
                    .ok_or(SupervisorError::TaskNotFound(token.task_id))?;
                match &task.state {
                    TaskState::Assigned { assignment } if assignment.token() == token => {
                        assignment.clone()
                    }
                    state => {
                        return Err(SupervisorError::InvalidTransition {
                            task_id: token.task_id,
                            action: "start worker",
                            state: state.name(),
                        });
                    }
                }
            };

            store
                .tasks
                .get_mut(&token.task_id)
                .expect("task was checked above")
                .state = TaskState::Running {
                assignment: assignment.clone(),
                started_at: Instant::now(),
            };
            vec![store.append_event(
                token.task_id,
                EventKind::WorkerStarted {
                    worker_id: assignment.worker_id,
                    assignment_id: assignment.id,
                    generation: assignment.generation,
                },
                self.state.event_history_limit,
            )?]
        };
        self.publish(events);
        Ok(())
    }

    pub async fn expire_assignment(
        &self,
        token: AssignmentToken,
    ) -> Result<TransitionOutcome, SupervisorError> {
        let events = {
            let mut store = self.state.store.lock().await;
            let assignment = {
                let task = store
                    .tasks
                    .get(&token.task_id)
                    .ok_or(SupervisorError::TaskNotFound(token.task_id))?;
                match task.state.active_assignment() {
                    Some(assignment)
                        if assignment.token() == token
                            && matches!(
                                task.state,
                                TaskState::Assigned { .. } | TaskState::Running { .. }
                            ) =>
                    {
                        assignment.clone()
                    }
                    _ => return Ok(TransitionOutcome::Ignored),
                }
            };

            let reason = FailureReason::DeadlineExceeded;
            store
                .tasks
                .get_mut(&token.task_id)
                .expect("task was checked above")
                .state = TaskState::Recovering {
                expired: assignment.clone(),
                reason: reason.clone(),
                next_generation: assignment.generation.next(),
            };
            vec![
                store.append_event(
                    token.task_id,
                    EventKind::WorkerFailed {
                        worker_id: assignment.worker_id.clone(),
                        assignment_id: assignment.id,
                        generation: assignment.generation,
                        reason,
                    },
                    self.state.event_history_limit,
                )?,
                store.append_event(
                    token.task_id,
                    EventKind::AssignmentExpired {
                        worker_id: assignment.worker_id,
                        assignment_id: assignment.id,
                        generation: assignment.generation,
                    },
                    self.state.event_history_limit,
                )?,
            ]
        };
        self.publish(events);
        Ok(TransitionOutcome::Applied)
    }

    pub async fn record_worker_failure(
        &self,
        token: AssignmentToken,
        reason: FailureReason,
    ) -> Result<TransitionOutcome, SupervisorError> {
        let events = {
            let mut store = self.state.store.lock().await;
            let assignment = {
                let task = store
                    .tasks
                    .get(&token.task_id)
                    .ok_or(SupervisorError::TaskNotFound(token.task_id))?;
                match task.state.active_assignment() {
                    Some(assignment)
                        if assignment.token() == token
                            && matches!(
                                task.state,
                                TaskState::Assigned { .. } | TaskState::Running { .. }
                            ) =>
                    {
                        assignment.clone()
                    }
                    _ => return Ok(TransitionOutcome::Ignored),
                }
            };

            store
                .tasks
                .get_mut(&token.task_id)
                .expect("task was checked above")
                .state = TaskState::Recovering {
                expired: assignment.clone(),
                reason: reason.clone(),
                next_generation: assignment.generation.next(),
            };
            vec![store.append_event(
                token.task_id,
                EventKind::WorkerFailed {
                    worker_id: assignment.worker_id,
                    assignment_id: assignment.id,
                    generation: assignment.generation,
                    reason,
                },
                self.state.event_history_limit,
            )?]
        };
        self.publish(events);
        Ok(TransitionOutcome::Applied)
    }

    pub async fn submit_result(
        &self,
        token: AssignmentToken,
        worker_id: WorkerId,
        output: WorkerOutput,
    ) -> Result<(Mission, Submission), SupervisorError> {
        let accepted = {
            let mut store = self.state.store.lock().await;
            let validation = {
                let task = store
                    .tasks
                    .get(&token.task_id)
                    .ok_or(SupervisorError::TaskNotFound(token.task_id))?;
                validate_submission(task, token, &worker_id)
            };

            if let Err(reason) = validation {
                let current = store
                    .tasks
                    .get(&token.task_id)
                    .expect("task was checked above")
                    .last_generation;
                let event_kind = match reason.clone() {
                    SubmissionRejection::StaleGeneration { submitted, current } => {
                        EventKind::StaleSubmissionRejected {
                            worker_id,
                            assignment_id: token.assignment_id,
                            submitted,
                            current,
                        }
                    }
                    other => EventKind::SubmissionRejected {
                        worker_id,
                        assignment_id: token.assignment_id,
                        generation: token.generation,
                        reason: other,
                    },
                };
                let event = store.append_event(
                    token.task_id,
                    event_kind,
                    self.state.event_history_limit,
                )?;
                (Err(reason), vec![event], current)
            } else {
                let (assignment, mission) = {
                    let task = store
                        .tasks
                        .get(&token.task_id)
                        .expect("task was checked above");
                    let assignment = match &task.state {
                        TaskState::Running { assignment, .. } => assignment.clone(),
                        _ => unreachable!("validation only accepts running tasks"),
                    };
                    (assignment, task.mission.clone())
                };
                let submission_id = SubmissionId(store.next_submission_id);
                store.next_submission_id = store
                    .next_submission_id
                    .checked_add(1)
                    .expect("submission ID overflow");
                let submission = Submission {
                    id: submission_id,
                    token,
                    worker_id: worker_id.clone(),
                    output,
                    received_at: SystemTime::now(),
                };
                store
                    .tasks
                    .get_mut(&token.task_id)
                    .expect("task was checked above")
                    .state = TaskState::Verifying {
                    assignment: assignment.clone(),
                    submission: submission.clone(),
                };
                let events = vec![
                    store.append_event(
                        token.task_id,
                        EventKind::SubmissionReceived {
                            worker_id,
                            submission_id,
                            assignment_id: assignment.id,
                            generation: assignment.generation,
                        },
                        self.state.event_history_limit,
                    )?,
                    store.append_event(
                        token.task_id,
                        EventKind::VerificationStarted { submission_id },
                        self.state.event_history_limit,
                    )?,
                ];
                (Ok((mission, submission)), events, assignment.generation)
            }
        };

        let (result, events, _current_generation) = accepted;
        self.publish(events);
        result.map_err(SupervisorError::SubmissionRejected)
    }

    pub async fn record_verification(
        &self,
        submission: &Submission,
        result: Result<(), VerificationError>,
    ) -> Result<(), SupervisorError> {
        let events = {
            let mut store = self.state.store.lock().await;
            let assignment = {
                let task = store
                    .tasks
                    .get(&submission.token.task_id)
                    .ok_or(SupervisorError::TaskNotFound(submission.token.task_id))?;
                match &task.state {
                    TaskState::Verifying {
                        assignment,
                        submission: active,
                    } if assignment.token() == submission.token && active.id == submission.id => {
                        assignment.clone()
                    }
                    state => {
                        return Err(SupervisorError::InvalidTransition {
                            task_id: submission.token.task_id,
                            action: "record verification",
                            state: state.name(),
                        });
                    }
                }
            };

            match result {
                Ok(()) => {
                    let now = SystemTime::now();
                    store
                        .tasks
                        .get_mut(&submission.token.task_id)
                        .expect("task was checked above")
                        .state = TaskState::Completed {
                        accepted: VerifiedOutput {
                            submission: submission.clone(),
                            verified_at: now,
                        },
                        completed_at: now,
                    };
                    vec![
                        store.append_event(
                            submission.token.task_id,
                            EventKind::VerificationPassed {
                                submission_id: submission.id,
                            },
                            self.state.event_history_limit,
                        )?,
                        store.append_event(
                            submission.token.task_id,
                            EventKind::TaskCompleted {
                                submission_id: submission.id,
                                generation: submission.token.generation,
                            },
                            self.state.event_history_limit,
                        )?,
                    ]
                }
                Err(VerificationError::Rejected { code }) => {
                    let reason = FailureReason::VerificationRejected { code: code.clone() };
                    store
                        .tasks
                        .get_mut(&submission.token.task_id)
                        .expect("task was checked above")
                        .state = TaskState::Recovering {
                        expired: assignment.clone(),
                        reason,
                        next_generation: assignment.generation.next(),
                    };
                    vec![store.append_event(
                        submission.token.task_id,
                        EventKind::VerificationFailed {
                            submission_id: submission.id,
                            code,
                        },
                        self.state.event_history_limit,
                    )?]
                }
            }
        };
        self.publish(events);
        Ok(())
    }

    pub async fn submit_and_verify(
        &self,
        token: AssignmentToken,
        worker_id: WorkerId,
        output: WorkerOutput,
    ) -> Result<(), SupervisorError> {
        let (mission, submission) = self.submit_result(token, worker_id, output).await?;

        // Verification deliberately occurs after submit_result released the store lock.
        let result = self.verifier.verify(&mission, &submission.output);
        self.record_verification(&submission, result).await
    }

    pub async fn fail_if_recovering(
        &self,
        task_id: TaskId,
    ) -> Result<TransitionOutcome, SupervisorError> {
        let events = {
            let mut store = self.state.store.lock().await;
            let last_reason = {
                let task = store
                    .tasks
                    .get(&task_id)
                    .ok_or(SupervisorError::TaskNotFound(task_id))?;
                match &task.state {
                    TaskState::Pending => None,
                    TaskState::Recovering { reason, .. } => Some(reason.clone()),
                    TaskState::Completed { .. } | TaskState::Failed { .. } => {
                        return Ok(TransitionOutcome::Ignored);
                    }
                    _ => {
                        return Err(SupervisorError::InvalidTransition {
                            task_id,
                            action: "fail exhausted task",
                            state: task.state.name(),
                        });
                    }
                }
            };
            let reason = TerminalFailure::WorkersExhausted { last_reason };
            store
                .tasks
                .get_mut(&task_id)
                .expect("task was checked above")
                .state = TaskState::Failed {
                reason: reason.clone(),
                failed_at: SystemTime::now(),
            };
            vec![store.append_event(
                task_id,
                EventKind::TaskFailed { reason },
                self.state.event_history_limit,
            )?]
        };
        self.publish(events);
        Ok(TransitionOutcome::Applied)
    }

    pub async fn run_task(
        self: &Arc<Self>,
        task_id: TaskId,
        workers: Vec<Arc<dyn Worker>>,
        lease_duration: Duration,
    ) -> Result<(), SupervisorError> {
        for worker in workers {
            let worker_id = worker.id();
            let assignment = self
                .assign_next_worker(task_id, worker_id.clone(), lease_duration)
                .await?;
            let token = assignment.token();
            self.mark_worker_started(token).await?;
            let mission = self.snapshot(task_id).await?.mission;

            let worker_supervisor = Arc::clone(self);
            let worker_token = token;
            let worker_task = tokio::spawn(async move {
                let result = worker
                    .execute(WorkRequest {
                        mission,
                        token: worker_token,
                    })
                    .await;
                match result {
                    Ok(output) => {
                        worker_supervisor
                            .submit_and_verify(worker_token, worker_id, output)
                            .await
                    }
                    Err(WorkerError::Execution { message }) => worker_supervisor
                        .record_worker_failure(worker_token, FailureReason::WorkerError { message })
                        .await
                        .map(|_| ()),
                }
            });

            let deadline_supervisor = Arc::clone(self);
            let deadline_token = token;
            let deadline = assignment.deadline;
            let deadline_task = tokio::spawn(async move {
                sleep_until(deadline).await;
                deadline_supervisor.expire_assignment(deadline_token).await
            });

            tokio::pin!(worker_task);
            tokio::pin!(deadline_task);

            tokio::select! {
                joined = &mut worker_task => {
                    self.handle_worker_join(token, joined).await?;
                    // Dropping the JoinHandle detaches the deadline task. It still
                    // fires later and proves terminal/current-token protection.
                }
                joined = &mut deadline_task => {
                    let deadline_outcome = joined.map_err(|error| SupervisorError::BackgroundTask {
                        role: "deadline",
                        message: error.to_string(),
                    })??;
                    if deadline_outcome == TransitionOutcome::Ignored {
                        let joined = worker_task.await;
                        self.handle_worker_join(token, joined).await?;
                    }
                    // If expiry applied, dropping the worker JoinHandle detaches
                    // the still-running worker so its eventual result is checked
                    // and rejected through the normal submission path.
                }
            }

            match self.snapshot(task_id).await?.state {
                TaskState::Completed { .. } | TaskState::Failed { .. } => return Ok(()),
                TaskState::Recovering { .. } => continue,
                state => {
                    return Err(SupervisorError::InvalidTransition {
                        task_id,
                        action: "continue supervision",
                        state: state.name(),
                    });
                }
            }
        }

        self.fail_if_recovering(task_id).await?;
        Ok(())
    }

    async fn handle_worker_join(
        &self,
        token: AssignmentToken,
        joined: Result<Result<(), SupervisorError>, JoinError>,
    ) -> Result<(), SupervisorError> {
        match joined {
            Ok(result) => result,
            Err(error) => self
                .record_worker_failure(token, FailureReason::WorkerPanicked)
                .await
                .map(|_| ())
                .map_err(|supervisor_error| SupervisorError::BackgroundTask {
                    role: "worker",
                    message: format!("{error}; recovery failed: {supervisor_error}"),
                }),
        }
    }

    fn publish(&self, events: Vec<MeldEvent>) {
        for event in events {
            trace_event(&event);
            let _ = self.state.event_sender.send(event);
        }
    }
}

fn validate_submission(
    task: &TaskRecord,
    token: AssignmentToken,
    worker_id: &WorkerId,
) -> Result<(), SubmissionRejection> {
    if token.task_id != task.id {
        return Err(SubmissionRejection::WrongTask);
    }
    if token.generation < task.last_generation {
        return Err(SubmissionRejection::StaleGeneration {
            submitted: token.generation,
            current: task.last_generation,
        });
    }

    match &task.state {
        TaskState::Running { assignment, .. }
            if assignment.token() == token && assignment.worker_id == *worker_id =>
        {
            Ok(())
        }
        TaskState::Completed { .. } | TaskState::Failed { .. } => {
            Err(SubmissionRejection::TaskAlreadyTerminal)
        }
        TaskState::Running { .. } => Err(SubmissionRejection::WrongAssignment),
        state => Err(SubmissionRejection::InvalidState {
            state: state.name(),
        }),
    }
}

fn trace_event(event: &MeldEvent) {
    match &event.kind {
        EventKind::TaskAssigned {
            worker_id,
            assignment_id,
            generation,
        }
        | EventKind::WorkerStarted {
            worker_id,
            assignment_id,
            generation,
        }
        | EventKind::AssignmentExpired {
            worker_id,
            assignment_id,
            generation,
        } => tracing::info!(
            task_id = event.task_id.0,
            worker_id = %worker_id,
            assignment_id = assignment_id.0,
            generation = generation.0,
            sequence = event.sequence,
            event = event.kind.name(),
            "Meld lifecycle event"
        ),
        EventKind::WorkerFailed {
            worker_id,
            assignment_id,
            generation,
            reason,
        } => tracing::warn!(
            task_id = event.task_id.0,
            worker_id = %worker_id,
            assignment_id = assignment_id.0,
            generation = generation.0,
            reason = %reason,
            sequence = event.sequence,
            event = event.kind.name(),
            "Meld worker failure"
        ),
        EventKind::StaleSubmissionRejected {
            worker_id,
            assignment_id,
            submitted,
            current,
        } => tracing::warn!(
            task_id = event.task_id.0,
            worker_id = %worker_id,
            assignment_id = assignment_id.0,
            submitted_generation = submitted.0,
            current_generation = current.0,
            sequence = event.sequence,
            event = event.kind.name(),
            "Meld rejected stale submission"
        ),
        _ => tracing::info!(
            task_id = event.task_id.0,
            sequence = event.sequence,
            event = event.kind.name(),
            "Meld lifecycle event"
        ),
    }
}
