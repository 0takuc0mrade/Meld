use std::time::SystemTime;

use crate::domain::{
    AssignmentId, FailureReason, Generation, SubmissionId, SubmissionRejection, TaskId,
    TerminalFailure, VerificationCode, WorkerId, WorkerOutput,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeldEvent {
    pub sequence: u64,
    pub task_id: TaskId,
    pub occurred_at: SystemTime,
    pub kind: EventKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventKind {
    TaskCreated,
    TaskAssigned {
        worker_id: WorkerId,
        assignment_id: AssignmentId,
        generation: Generation,
    },
    WorkerStarted {
        worker_id: WorkerId,
        assignment_id: AssignmentId,
        generation: Generation,
    },
    AgentExecutionStarted {
        worker_id: WorkerId,
        assignment_id: AssignmentId,
        generation: Generation,
        provider: &'static str,
        model: String,
    },
    AgentOutputParsed {
        worker_id: WorkerId,
        assignment_id: AssignmentId,
        generation: Generation,
        provider: &'static str,
        model: String,
        duration_ms: u64,
        output: WorkerOutput,
    },
    AgentExecutionFailed {
        worker_id: WorkerId,
        assignment_id: AssignmentId,
        generation: Generation,
        provider: &'static str,
        model: String,
        duration_ms: u64,
        reason: String,
    },
    WorkerFailed {
        worker_id: WorkerId,
        assignment_id: AssignmentId,
        generation: Generation,
        reason: FailureReason,
    },
    AssignmentExpired {
        worker_id: WorkerId,
        assignment_id: AssignmentId,
        generation: Generation,
    },
    TaskReassigned {
        from: WorkerId,
        to: WorkerId,
        assignment_id: AssignmentId,
        generation: Generation,
    },
    SubmissionReceived {
        worker_id: WorkerId,
        submission_id: SubmissionId,
        assignment_id: AssignmentId,
        generation: Generation,
    },
    SubmissionRejected {
        worker_id: WorkerId,
        assignment_id: AssignmentId,
        generation: Generation,
        reason: SubmissionRejection,
    },
    StaleSubmissionRejected {
        worker_id: WorkerId,
        assignment_id: AssignmentId,
        submitted: Generation,
        current: Generation,
    },
    VerificationStarted {
        submission_id: SubmissionId,
    },
    VerificationFailed {
        submission_id: SubmissionId,
        code: VerificationCode,
    },
    VerificationPassed {
        submission_id: SubmissionId,
    },
    TaskCompleted {
        submission_id: SubmissionId,
        generation: Generation,
    },
    TaskFailed {
        reason: TerminalFailure,
    },
}

impl EventKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::TaskCreated => "task.created",
            Self::TaskAssigned { .. } => "task.assigned",
            Self::WorkerStarted { .. } => "worker.started",
            Self::AgentExecutionStarted { .. } => "agent.execution.started",
            Self::AgentOutputParsed { .. } => "agent.output.parsed",
            Self::AgentExecutionFailed { .. } => "agent.execution.failed",
            Self::WorkerFailed { reason, .. } => match reason {
                FailureReason::DeadlineExceeded => "worker.timeout",
                _ => "worker.failed",
            },
            Self::AssignmentExpired { .. } => "assignment.expired",
            Self::TaskReassigned { .. } => "task.reassigned",
            Self::SubmissionReceived { .. } => "submission.received",
            Self::SubmissionRejected { .. } => "submission.rejected",
            Self::StaleSubmissionRejected { .. } => "submission.stale_rejected",
            Self::VerificationStarted { .. } => "verification.started",
            Self::VerificationFailed { .. } => "verification.failed",
            Self::VerificationPassed { .. } => "verification.passed",
            Self::TaskCompleted { .. } => "task.completed",
            Self::TaskFailed { .. } => "task.failed",
        }
    }
}
