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

        if let Some(incident) = &mission.incident {
            let analysis =
                output
                    .incident_analysis
                    .as_ref()
                    .ok_or(VerificationError::Rejected {
                        code: VerificationCode::IncidentAnalysisMissing,
                    })?;

            if !analysis
                .affected_component
                .trim()
                .eq_ignore_ascii_case(&incident.verification.expected_component)
            {
                return Err(VerificationError::Rejected {
                    code: VerificationCode::AffectedComponentMismatch,
                });
            }

            if analysis.onset.trim() != incident.verification.expected_onset {
                return Err(VerificationError::Rejected {
                    code: VerificationCode::IncidentOnsetMismatch,
                });
            }

            let evidence_is_known = analysis.evidence_ids.iter().all(|candidate| {
                incident
                    .records
                    .iter()
                    .any(|record| record.id.eq_ignore_ascii_case(candidate.trim()))
            });
            if !evidence_is_known {
                return Err(VerificationError::Rejected {
                    code: VerificationCode::UnknownIncidentEvidence,
                });
            }

            let contains_required_evidence = incident
                .verification
                .required_evidence_ids
                .iter()
                .all(|required| {
                    analysis
                        .evidence_ids
                        .iter()
                        .any(|candidate| candidate.trim().eq_ignore_ascii_case(required))
                });
            if !contains_required_evidence {
                return Err(VerificationError::Rejected {
                    code: VerificationCode::RequiredIncidentEvidenceMissing,
                });
            }
        }

        Ok(())
    }
}
