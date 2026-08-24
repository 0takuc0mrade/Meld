use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use meld::domain::{
    FailureReason, Generation, Mission, SubmissionRejection, TaskState, VerificationError,
    WorkerId, WorkerOutput,
};
use meld::events::EventKind;
use meld::supervisor::{AppState, Supervisor, SupervisorError};
use meld::verifier::{DeterministicVerifier, Verifier};
use meld::worker::{ControlledDelayWorker, ErrorWorker, PanicWorker, SuccessfulWorker, Worker};

fn accepted_output(worker: &str) -> WorkerOutput {
    WorkerOutput::accepted_fixture(worker)
}

fn invalid_output() -> WorkerOutput {
    WorkerOutput {
        summary: "This answer discusses generation but omits a required concept.".to_owned(),
        evidence: vec!["It is structurally valid but semantically unacceptable.".to_owned()],
    }
}

fn supervisor() -> Arc<Supervisor> {
    Arc::new(Supervisor::new(
        Arc::new(AppState::default()),
        Arc::new(DeterministicVerifier),
    ))
}

fn worker<W: Worker + 'static>(worker: W) -> Arc<dyn Worker> {
    Arc::new(worker)
}

fn event_names(snapshot: &meld::supervisor::TaskSnapshot) -> Vec<&'static str> {
    snapshot
        .events
        .iter()
        .map(|event| event.kind.name())
        .collect()
}

fn assert_sequences_are_strictly_increasing(snapshot: &meld::supervisor::TaskSnapshot) {
    assert!(
        snapshot
            .events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
}

#[tokio::test]
async fn successful_lifecycle_is_verified_before_completion() {
    let supervisor = supervisor();
    let task_id = supervisor.create_task(Mission::fixture()).await.unwrap();

    supervisor
        .run_task(
            task_id,
            vec![worker(SuccessfulWorker::new(
                "agent-a",
                accepted_output("agent-a"),
            ))],
            Duration::from_secs(30),
        )
        .await
        .unwrap();

    let snapshot = supervisor.snapshot(task_id).await.unwrap();
    let TaskState::Completed { accepted, .. } = &snapshot.state else {
        panic!("expected completed task, got {:?}", snapshot.state);
    };
    assert_eq!(accepted.submission.token.generation, Generation(1));
    assert_eq!(accepted.submission.worker_id, WorkerId::new("agent-a"));
    assert_eq!(
        event_names(&snapshot),
        vec![
            "task.created",
            "task.assigned",
            "worker.started",
            "submission.received",
            "verification.started",
            "verification.passed",
            "task.completed",
        ]
    );
    assert_sequences_are_strictly_increasing(&snapshot);
}

#[tokio::test]
async fn worker_error_recovers_and_reassigns_a_fresh_generation() {
    let supervisor = supervisor();
    let task_id = supervisor.create_task(Mission::fixture()).await.unwrap();

    supervisor
        .run_task(
            task_id,
            vec![
                worker(ErrorWorker::new("agent-a", "provider unavailable")),
                worker(SuccessfulWorker::new("agent-b", accepted_output("agent-b"))),
            ],
            Duration::from_secs(30),
        )
        .await
        .unwrap();

    let snapshot = supervisor.snapshot(task_id).await.unwrap();
    let TaskState::Completed { accepted, .. } = &snapshot.state else {
        panic!("expected recovery to complete, got {:?}", snapshot.state);
    };
    assert_eq!(snapshot.last_generation, Generation(2));
    assert_eq!(accepted.submission.token.generation, Generation(2));
    assert_eq!(accepted.submission.worker_id, WorkerId::new("agent-b"));

    let names = event_names(&snapshot);
    assert!(names.contains(&"worker.failed"));
    assert!(names.contains(&"task.reassigned"));
    assert!(names.contains(&"task.completed"));
}

#[tokio::test]
async fn worker_panic_is_contained_and_recovered() {
    let supervisor = supervisor();
    let task_id = supervisor.create_task(Mission::fixture()).await.unwrap();

    supervisor
        .run_task(
            task_id,
            vec![
                worker(PanicWorker::new("agent-a")),
                worker(SuccessfulWorker::new("agent-b", accepted_output("agent-b"))),
            ],
            Duration::from_secs(30),
        )
        .await
        .unwrap();

    let snapshot = supervisor.snapshot(task_id).await.unwrap();
    assert!(matches!(snapshot.state, TaskState::Completed { .. }));
    assert!(snapshot.events.iter().any(|event| matches!(
        &event.kind,
        EventKind::WorkerFailed {
            reason: FailureReason::WorkerPanicked,
            ..
        }
    )));
}

#[derive(Default)]
struct CountingVerifier {
    calls: AtomicUsize,
    inner: DeterministicVerifier,
}

impl Verifier for CountingVerifier {
    fn verify(&self, mission: &Mission, output: &WorkerOutput) -> Result<(), VerificationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.verify(mission, output)
    }
}

#[tokio::test(start_paused = true)]
async fn late_generation_one_is_rejected_after_generation_two_completes() {
    let verifier = Arc::new(CountingVerifier::default());
    let supervisor = Arc::new(Supervisor::new(
        Arc::new(AppState::default()),
        verifier.clone(),
    ));
    let task_id = supervisor.create_task(Mission::fixture()).await.unwrap();
    let late_worker = ControlledDelayWorker::new(
        SuccessfulWorker::new("agent-a", accepted_output("agent-a")),
        Duration::from_secs(20),
    );

    let run = tokio::spawn({
        let supervisor = Arc::clone(&supervisor);
        async move {
            supervisor
                .run_task(
                    task_id,
                    vec![
                        worker(late_worker),
                        worker(SuccessfulWorker::new("agent-b", accepted_output("agent-b"))),
                    ],
                    Duration::from_secs(10),
                )
                .await
        }
    });

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(10)).await;
    run.await.unwrap().unwrap();

    let completed = supervisor.snapshot(task_id).await.unwrap();
    let TaskState::Completed { accepted, .. } = &completed.state else {
        panic!(
            "expected replacement worker to complete, got {:?}",
            completed.state
        );
    };
    assert_eq!(accepted.submission.worker_id, WorkerId::new("agent-b"));
    assert_eq!(accepted.submission.token.generation, Generation(2));
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 1);

    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;

    let after_stale_return = supervisor.snapshot(task_id).await.unwrap();
    let TaskState::Completed { accepted, .. } = &after_stale_return.state else {
        panic!("stale return changed terminal state");
    };
    assert_eq!(accepted.submission.worker_id, WorkerId::new("agent-b"));
    assert_eq!(accepted.submission.token.generation, Generation(2));
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 1);
    assert!(after_stale_return.events.iter().any(|event| matches!(
        event.kind,
        EventKind::StaleSubmissionRejected {
            submitted: Generation(1),
            current: Generation(2),
            ..
        }
    )));
}

#[tokio::test]
async fn verification_rejection_recovers_with_the_same_supervisor_path() {
    let supervisor = supervisor();
    let task_id = supervisor.create_task(Mission::fixture()).await.unwrap();

    supervisor
        .run_task(
            task_id,
            vec![
                worker(SuccessfulWorker::new("agent-a", invalid_output())),
                worker(SuccessfulWorker::new("agent-b", accepted_output("agent-b"))),
            ],
            Duration::from_secs(30),
        )
        .await
        .unwrap();

    let snapshot = supervisor.snapshot(task_id).await.unwrap();
    let TaskState::Completed { accepted, .. } = &snapshot.state else {
        panic!("expected retry after rejection, got {:?}", snapshot.state);
    };
    assert_eq!(accepted.submission.token.generation, Generation(2));
    assert!(event_names(&snapshot).contains(&"verification.failed"));
    assert!(event_names(&snapshot).contains(&"task.reassigned"));
}

#[tokio::test(start_paused = true)]
async fn result_wins_then_detached_deadline_becomes_a_no_op() {
    let supervisor = supervisor();
    let task_id = supervisor.create_task(Mission::fixture()).await.unwrap();

    supervisor
        .run_task(
            task_id,
            vec![worker(SuccessfulWorker::new(
                "agent-a",
                accepted_output("agent-a"),
            ))],
            Duration::from_secs(10),
        )
        .await
        .unwrap();

    let before_deadline = supervisor.snapshot(task_id).await.unwrap();
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    let after_deadline = supervisor.snapshot(task_id).await.unwrap();

    assert_eq!(before_deadline.state, after_deadline.state);
    assert_eq!(before_deadline.events, after_deadline.events);
    assert!(!event_names(&after_deadline).contains(&"assignment.expired"));
}

#[tokio::test(start_paused = true)]
async fn exact_deadline_race_is_safe_whichever_side_wins() {
    let supervisor = supervisor();
    let task_id = supervisor.create_task(Mission::fixture()).await.unwrap();
    let lease = Duration::from_secs(10);

    let run = tokio::spawn({
        let supervisor = Arc::clone(&supervisor);
        async move {
            supervisor
                .run_task(
                    task_id,
                    vec![
                        worker(ControlledDelayWorker::new(
                            SuccessfulWorker::new("agent-a", accepted_output("agent-a")),
                            lease,
                        )),
                        worker(SuccessfulWorker::new("agent-b", accepted_output("agent-b"))),
                    ],
                    lease,
                )
                .await
        }
    });

    tokio::task::yield_now().await;
    tokio::time::advance(lease).await;
    run.await.unwrap().unwrap();
    tokio::task::yield_now().await;

    let snapshot = supervisor.snapshot(task_id).await.unwrap();
    let TaskState::Completed { accepted, .. } = &snapshot.state else {
        panic!("deadline race left invalid state: {:?}", snapshot.state);
    };
    assert!(matches!(
        accepted.submission.token.generation,
        Generation(1) | Generation(2)
    ));
    assert_eq!(
        snapshot
            .events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::TaskCompleted { .. }))
            .count(),
        1
    );
    assert_sequences_are_strictly_increasing(&snapshot);
}

#[tokio::test]
async fn completed_and_failed_states_cannot_be_reopened_by_late_work() {
    let completed_supervisor = supervisor();
    let completed_id = completed_supervisor
        .create_task(Mission::fixture())
        .await
        .unwrap();
    completed_supervisor
        .run_task(
            completed_id,
            vec![worker(SuccessfulWorker::new(
                "agent-a",
                accepted_output("agent-a"),
            ))],
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let completed_snapshot = completed_supervisor.snapshot(completed_id).await.unwrap();
    let TaskState::Completed { accepted, .. } = &completed_snapshot.state else {
        panic!("expected completed state");
    };
    let completed_token = accepted.submission.token;
    let completed_state = completed_snapshot.state.clone();

    let error = completed_supervisor
        .submit_and_verify(
            completed_token,
            WorkerId::new("agent-a"),
            accepted_output("late-agent-a"),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SupervisorError::SubmissionRejected(SubmissionRejection::TaskAlreadyTerminal)
    ));
    assert_eq!(
        completed_supervisor
            .snapshot(completed_id)
            .await
            .unwrap()
            .state,
        completed_state
    );

    let failed_supervisor = supervisor();
    let failed_id = failed_supervisor
        .create_task(Mission::fixture())
        .await
        .unwrap();
    failed_supervisor
        .run_task(
            failed_id,
            vec![worker(ErrorWorker::new("agent-a", "fatal fixture error"))],
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let failed_snapshot = failed_supervisor.snapshot(failed_id).await.unwrap();
    assert!(matches!(failed_snapshot.state, TaskState::Failed { .. }));
    let EventKind::TaskAssigned {
        assignment_id,
        generation,
        ..
    } = failed_snapshot
        .events
        .iter()
        .find(|event| matches!(event.kind, EventKind::TaskAssigned { .. }))
        .unwrap()
        .kind
        .clone()
    else {
        unreachable!();
    };
    let failed_state = failed_snapshot.state.clone();
    let error = failed_supervisor
        .submit_and_verify(
            meld::domain::AssignmentToken {
                task_id: failed_id,
                assignment_id,
                generation,
            },
            WorkerId::new("agent-a"),
            accepted_output("late-agent-a"),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SupervisorError::SubmissionRejected(SubmissionRejection::TaskAlreadyTerminal)
    ));
    assert_eq!(
        failed_supervisor.snapshot(failed_id).await.unwrap().state,
        failed_state
    );
}

#[tokio::test]
async fn event_history_is_bounded_without_becoming_authoritative_state() {
    let supervisor = Arc::new(Supervisor::new(
        Arc::new(AppState::new(3, 8)),
        Arc::new(DeterministicVerifier),
    ));
    let task_id = supervisor.create_task(Mission::fixture()).await.unwrap();
    supervisor
        .run_task(
            task_id,
            vec![worker(SuccessfulWorker::new(
                "agent-a",
                accepted_output("agent-a"),
            ))],
            Duration::from_secs(30),
        )
        .await
        .unwrap();

    let snapshot = supervisor.snapshot(task_id).await.unwrap();
    assert!(matches!(snapshot.state, TaskState::Completed { .. }));
    assert_eq!(snapshot.events.len(), 3);
    assert_eq!(
        event_names(&snapshot),
        vec![
            "verification.started",
            "verification.passed",
            "task.completed"
        ]
    );
}
