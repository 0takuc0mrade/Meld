#![cfg(feature = "rig-worker")]

use std::sync::Arc;
use std::time::Duration;

use meld::domain::{
    AssignmentId, AssignmentToken, Generation, Mission, TaskId, TaskState, WorkRequest, WorkerError,
};
use meld::events::EventKind;
use meld::rig_worker::{
    AnalysisError, AnalysisFuture, IncidentAnalysisProposal, IncidentAnalyzer, RigWorker,
};
use meld::supervisor::{AppState, Supervisor};
use meld::verifier::{DeterministicVerifier, Verifier};
use meld::worker::{ControlledDelayWorker, Worker};

#[derive(Clone)]
struct ScriptedAnalyzer {
    outcome: Result<IncidentAnalysisProposal, AnalysisError>,
}

#[derive(Clone)]
struct PendingAnalyzer;

impl ScriptedAnalyzer {
    fn successful(label: &str) -> Self {
        Self {
            outcome: Ok(valid_proposal(label)),
        }
    }
}

impl IncidentAnalyzer for ScriptedAnalyzer {
    fn analyze(&self, _prompt: String) -> AnalysisFuture {
        let outcome = self.outcome.clone();
        Box::pin(async move { outcome })
    }
}

impl IncidentAnalyzer for PendingAnalyzer {
    fn analyze(&self, _prompt: String) -> AnalysisFuture {
        Box::pin(std::future::pending())
    }
}

fn valid_proposal(label: &str) -> IncidentAnalysisProposal {
    IncidentAnalysisProposal {
        affected_component: "payments-api".to_owned(),
        onset: "2026-08-24T10:01:00Z".to_owned(),
        evidence_ids: vec!["EV-101".to_owned(), "EV-102".to_owned()],
        summary: format!(
            "{label} concluded that payments-api initiated the checkout incident at 2026-08-24T10:01:00Z."
        ),
    }
}

fn request() -> WorkRequest {
    WorkRequest::new(
        Mission::incident_fixture(),
        AssignmentToken {
            task_id: TaskId(1),
            assignment_id: AssignmentId(1),
            generation: Generation(1),
        },
    )
}

fn rig_worker(id: &str, outcome: Result<IncidentAnalysisProposal, AnalysisError>) -> RigWorker {
    RigWorker::with_analyzer(
        id,
        "test-provider",
        "test-model",
        Duration::from_secs(5),
        Arc::new(ScriptedAnalyzer { outcome }),
    )
}

#[tokio::test]
async fn valid_structured_agent_proposal_becomes_verified_worker_output() {
    let mission = Mission::incident_fixture();
    let worker = rig_worker("Agent A", Ok(valid_proposal("Agent A")));

    let output = worker.execute(request()).await.unwrap();

    assert_eq!(output.evidence.len(), 2);
    assert_eq!(
        output
            .incident_analysis
            .as_ref()
            .unwrap()
            .affected_component,
        "payments-api"
    );
    DeterministicVerifier.verify(&mission, &output).unwrap();
}

#[tokio::test]
async fn malformed_agent_output_becomes_a_typed_worker_error() {
    let worker = rig_worker("Agent A", Err(AnalysisError::InvalidOutput));

    let error = worker.execute(request()).await.unwrap_err();

    assert_eq!(
        error,
        WorkerError::Execution {
            message: "model returned invalid structured output".to_owned()
        }
    );
}

#[tokio::test]
async fn provider_failure_becomes_a_typed_worker_error() {
    let worker = rig_worker("Agent A", Err(AnalysisError::Provider));

    let error = worker.execute(request()).await.unwrap_err();

    assert_eq!(
        error,
        WorkerError::Execution {
            message: "model provider request failed".to_owned()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn provider_timeout_becomes_a_typed_worker_error() {
    let worker = RigWorker::with_analyzer(
        "Agent A",
        "test-provider",
        "test-model",
        Duration::from_secs(5),
        Arc::new(PendingAnalyzer),
    );
    let execution = tokio::spawn(worker.execute(request()));

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(5)).await;

    assert_eq!(
        execution.await.unwrap().unwrap_err(),
        WorkerError::Execution {
            message: "model provider request timed out".to_owned()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn controlled_delay_composes_around_rig_worker_after_real_execution() {
    let delayed = ControlledDelayWorker::new(
        rig_worker("Agent A", Ok(valid_proposal("Agent A"))),
        Duration::from_secs(10),
    );
    let execution = tokio::spawn(delayed.execute(request()));

    tokio::task::yield_now().await;
    assert!(!execution.is_finished());
    tokio::time::advance(Duration::from_secs(10)).await;

    assert!(execution.await.unwrap().is_ok());
}

#[tokio::test(start_paused = true)]
async fn two_rig_workers_use_normal_recovery_and_reject_generation_one_late() {
    let supervisor = Arc::new(Supervisor::new(
        Arc::new(AppState::default()),
        Arc::new(DeterministicVerifier),
    ));
    let task_id = supervisor
        .create_task(Mission::incident_fixture())
        .await
        .unwrap();
    let first = RigWorker::with_analyzer(
        "Worker A",
        "test-provider",
        "test-model",
        Duration::from_secs(5),
        Arc::new(ScriptedAnalyzer::successful("Worker A")),
    );
    let second = RigWorker::with_analyzer(
        "Worker B",
        "test-provider",
        "test-model",
        Duration::from_secs(5),
        Arc::new(ScriptedAnalyzer::successful("Worker B")),
    );

    let run = tokio::spawn({
        let supervisor = Arc::clone(&supervisor);
        async move {
            supervisor
                .run_task(
                    task_id,
                    vec![
                        Arc::new(ControlledDelayWorker::new(first, Duration::from_secs(20))),
                        Arc::new(second),
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
        panic!("expected Worker B to complete the incident mission");
    };
    assert_eq!(accepted.submission.worker_id.as_str(), "Worker B");
    assert_eq!(accepted.submission.token.generation, Generation(2));
    let first_parsed = completed
        .events
        .iter()
        .position(|event| {
            matches!(
                &event.kind,
                EventKind::AgentOutputParsed {
                    worker_id,
                    provider: "test-provider",
                    model,
                    output,
                    ..
                } if worker_id.as_str() == "Worker A"
                    && model == "test-model"
                    && output.incident_analysis.is_some()
            )
        })
        .expect("Worker A's parsed output should be recorded before its delay");
    let first_expired = completed
        .events
        .iter()
        .position(|event| {
            matches!(
                &event.kind,
                EventKind::AssignmentExpired { worker_id, .. }
                    if worker_id.as_str() == "Worker A"
            )
        })
        .expect("Worker A's lease should expire");
    assert!(first_parsed < first_expired);
    assert!(completed.events.iter().any(|event| matches!(
        &event.kind,
        EventKind::AgentOutputParsed {
            worker_id,
            provider: "test-provider",
            model,
            ..
        } if worker_id.as_str() == "Worker B" && model == "test-model"
    )));

    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;

    let after_late_return = supervisor.snapshot(task_id).await.unwrap();
    assert!(after_late_return.events.iter().any(|event| matches!(
        event.kind,
        EventKind::StaleSubmissionRejected {
            submitted: Generation(1),
            current: Generation(2),
            ..
        }
    )));
    let TaskState::Completed { accepted, .. } = after_late_return.state else {
        panic!("late generation one changed terminal state");
    };
    assert_eq!(accepted.submission.worker_id.as_str(), "Worker B");
}
