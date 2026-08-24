use crate::domain::{Mission, VerificationCode, VerificationError, WorkerOutput};

pub trait Verifier: Send + Sync {
    fn verify(&self, mission: &Mission, output: &WorkerOutput) -> Result<(), VerificationError>;
}

#[derive(Debug, Default)]
pub struct DeterministicVerifier;

impl Verifier for DeterministicVerifier {
    fn verify(&self, mission: &Mission, output: &WorkerOutput) -> Result<(), VerificationError> {
        let summary = output.summary.trim();
        if summary.chars().count() < mission.acceptance.minimum_summary_chars {
            return Err(VerificationError::Rejected {
                code: VerificationCode::SummaryTooShort,
            });
        }

        let normalized = summary.to_lowercase();
        if mission
            .acceptance
            .required_terms
            .iter()
            .any(|term| !normalized.contains(&term.to_lowercase()))
        {
            return Err(VerificationError::Rejected {
                code: VerificationCode::RequiredTermMissing,
            });
        }

        if output.evidence.len() < mission.acceptance.minimum_evidence_items
            || output.evidence.iter().any(|item| item.trim().is_empty())
        {
            return Err(VerificationError::Rejected {
                code: VerificationCode::InsufficientEvidence,
            });
        }

        Ok(())
    }
}
