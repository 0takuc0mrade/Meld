use std::fmt;
use std::time::SystemTime;

use tokio::time::Instant;

macro_rules! numeric_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

numeric_id!(TaskId);
numeric_id!(AssignmentId);
numeric_id!(SubmissionId);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Generation(pub u32);

impl Generation {
    pub fn next(self) -> Self {
        Self(
            self.0
                .checked_add(1)
                .expect("assignment generation overflow"),
        )
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WorkerId(String);

impl WorkerId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptanceCriteria {
    pub minimum_summary_chars: usize,
    pub required_terms: Vec<String>,
    pub minimum_evidence_items: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mission {
    pub title: String,
    pub objective: String,
    pub acceptance: AcceptanceCriteria,
}

impl Mission {
    pub fn fixture() -> Self {
        Self {
            title: "Reliability summary".to_owned(),
            objective: "Explain why assignment generations protect accepted work".to_owned(),
            acceptance: AcceptanceCriteria {
                minimum_summary_chars: 24,
                required_terms: vec!["generation".to_owned(), "stale".to_owned()],
                minimum_evidence_items: 1,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Assignment {
    pub id: AssignmentId,
    pub task_id: TaskId,
    pub worker_id: WorkerId,
    pub generation: Generation,
    pub issued_at: Instant,
    pub deadline: Instant,
}

impl Assignment {
    pub fn token(&self) -> AssignmentToken {
        AssignmentToken {
            task_id: self.task_id,
            assignment_id: self.id,
            generation: self.generation,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AssignmentToken {
    pub task_id: TaskId,
    pub assignment_id: AssignmentId,
    pub generation: Generation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerOutput {
    pub summary: String,
    pub evidence: Vec<String>,
}

impl WorkerOutput {
    pub fn accepted_fixture(label: &str) -> Self {
        Self {
            summary: format!(
                "{label}: a new generation makes an expired worker result stale and non-authoritative."
            ),
            evidence: vec!["The assignment token is checked under the state lock.".to_owned()],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Submission {
    pub id: SubmissionId,
    pub token: AssignmentToken,
    pub worker_id: WorkerId,
    pub output: WorkerOutput,
    pub received_at: SystemTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedOutput {
    pub submission: Submission,
    pub verified_at: SystemTime,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WorkerError {
    #[error("worker execution failed: {message}")]
    Execution { message: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerificationCode {
    SummaryTooShort,
    RequiredTermMissing,
    InsufficientEvidence,
}

impl fmt::Display for VerificationCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::SummaryTooShort => "summary_too_short",
            Self::RequiredTermMissing => "required_term_missing",
            Self::InsufficientEvidence => "insufficient_evidence",
        };
        formatter.write_str(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VerificationError {
    #[error("verification rejected output: {code}")]
    Rejected { code: VerificationCode },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FailureReason {
    WorkerError { message: String },
    DeadlineExceeded,
    WorkerPanicked,
    VerificationRejected { code: VerificationCode },
}

impl fmt::Display for FailureReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerError { message } => write!(formatter, "worker error: {message}"),
            Self::DeadlineExceeded => formatter.write_str("assignment deadline exceeded"),
            Self::WorkerPanicked => formatter.write_str("worker task panicked"),
            Self::VerificationRejected { code } => {
                write!(formatter, "verification rejected output: {code}")
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalFailure {
    WorkersExhausted { last_reason: Option<FailureReason> },
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SubmissionRejection {
    #[error("submission task does not match the authoritative task")]
    WrongTask,
    #[error("assignment generation {submitted} is stale; current generation is {current}")]
    StaleGeneration {
        submitted: Generation,
        current: Generation,
    },
    #[error("assignment token does not match the authoritative assignment")]
    WrongAssignment,
    #[error("task is already terminal")]
    TaskAlreadyTerminal,
    #[error("task is not accepting submissions while {state}")]
    InvalidState { state: &'static str },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskState {
    Pending,
    Assigned {
        assignment: Assignment,
    },
    Running {
        assignment: Assignment,
        started_at: Instant,
    },
    Recovering {
        expired: Assignment,
        reason: FailureReason,
        next_generation: Generation,
    },
    Verifying {
        assignment: Assignment,
        submission: Submission,
    },
    Completed {
        accepted: VerifiedOutput,
        completed_at: SystemTime,
    },
    Failed {
        reason: TerminalFailure,
        failed_at: SystemTime,
    },
}

impl TaskState {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Assigned { .. } => "assigned",
            Self::Running { .. } => "running",
            Self::Recovering { .. } => "recovering",
            Self::Verifying { .. } => "verifying",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
        }
    }

    pub fn active_assignment(&self) -> Option<&Assignment> {
        match self {
            Self::Assigned { assignment }
            | Self::Running { assignment, .. }
            | Self::Verifying { assignment, .. } => Some(assignment),
            Self::Pending
            | Self::Recovering { .. }
            | Self::Completed { .. }
            | Self::Failed { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkRequest {
    pub mission: Mission,
    pub token: AssignmentToken,
}
