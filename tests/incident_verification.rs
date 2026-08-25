use meld::domain::{IncidentAnalysis, Mission, VerificationCode, VerificationError, WorkerOutput};
use meld::verifier::{DeterministicVerifier, Verifier};

fn output() -> WorkerOutput {
    WorkerOutput::accepted_incident_fixture("deterministic worker")
}

fn rejection_code(output: &WorkerOutput) -> VerificationCode {
    let error = DeterministicVerifier
        .verify(&Mission::incident_fixture(), output)
        .unwrap_err();
    let VerificationError::Rejected { code } = error;
    code
}

#[test]
fn incident_policy_accepts_the_known_component_onset_and_evidence() {
    DeterministicVerifier
        .verify(&Mission::incident_fixture(), &output())
        .unwrap();
}

#[test]
fn incident_policy_rejects_the_wrong_component() {
    let mut output = output();
    output
        .incident_analysis
        .as_mut()
        .unwrap()
        .affected_component = "checkout-ui".to_owned();

    assert_eq!(
        rejection_code(&output),
        VerificationCode::AffectedComponentMismatch
    );
}

#[test]
fn incident_policy_rejects_an_unsupported_onset() {
    let mut output = output();
    output.incident_analysis.as_mut().unwrap().onset = "2026-08-24T10:03:00Z".to_owned();

    assert_eq!(
        rejection_code(&output),
        VerificationCode::IncidentOnsetMismatch
    );
}

#[test]
fn incident_policy_rejects_unknown_or_missing_support() {
    let mut unknown = output();
    unknown.incident_analysis.as_mut().unwrap().evidence_ids = vec![
        "EV-101".to_owned(),
        "EV-102".to_owned(),
        "EV-999".to_owned(),
    ];
    assert_eq!(
        rejection_code(&unknown),
        VerificationCode::UnknownIncidentEvidence
    );

    let mut missing = output();
    missing.incident_analysis = Some(IncidentAnalysis {
        affected_component: "payments-api".to_owned(),
        onset: "2026-08-24T10:01:00Z".to_owned(),
        evidence_ids: vec!["EV-101".to_owned()],
    });
    assert_eq!(
        rejection_code(&missing),
        VerificationCode::RequiredIncidentEvidenceMissing
    );
}
