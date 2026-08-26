//! Feature-gated Rig adapter for real, structured incident-analysis work.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Once};
use std::time::Duration;

use rig_agent::extractor::ExtractionError;
use rig_agent::prelude::*;
use rig_core::providers::gemini;

use crate::domain::{
    IncidentAnalysis, IncidentCase, Mission, WorkRequest, WorkerActivity, WorkerError, WorkerId,
    WorkerOutput,
};
use crate::worker::{ControlledDelayWorker, Worker, WorkerFuture};

const DEFAULT_MODEL: &str = "gemini-3.6-flash";
const DEFAULT_ASSIGNMENT_LEASE_MS: u64 = 35_000;
const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = 25_000;
const DEFAULT_AGENT_A_DELAY_MS: u64 = 65_000;
const MAX_OUTPUT_TOKENS: u64 = 2_048;

static INSTALL_RING_PROVIDER: Once = Once::new();

pub type AnalysisFuture =
    Pin<Box<dyn Future<Output = Result<IncidentAnalysisProposal, AnalysisError>> + Send + 'static>>;

/// Narrow model boundary used by `RigWorker` and deterministic offline tests.
pub trait IncidentAnalyzer: Send + Sync {
    fn analyze(&self, prompt: String) -> AnalysisFuture;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AnalysisError {
    #[error("model provider request failed")]
    Provider,
    #[error("model returned invalid structured output")]
    InvalidOutput,
}

#[derive(
    Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize, schemars::JsonSchema,
)]
pub struct IncidentAnalysisProposal {
    pub affected_component: String,
    pub onset: String,
    pub evidence_ids: Vec<String>,
    pub summary: String,
}

impl IncidentAnalysisProposal {
    fn into_worker_output(self, incident: &IncidentCase) -> WorkerOutput {
        let evidence = self
            .evidence_ids
            .iter()
            .map(|evidence_id| {
                incident
                    .records
                    .iter()
                    .find(|record| record.id.eq_ignore_ascii_case(evidence_id.trim()))
                    .map_or_else(
                        || format!("{evidence_id}: unknown incident record"),
                        |record| format!("{}: {}", record.id, record.observation),
                    )
            })
            .collect();

        WorkerOutput {
            summary: self.summary,
            evidence,
            incident_analysis: Some(IncidentAnalysis {
                affected_component: self.affected_component,
                onset: self.onset,
                evidence_ids: self.evidence_ids,
            }),
        }
    }
}

struct GeminiRigAnalyzer {
    api_key: String,
    model: String,
}

impl GeminiRigAnalyzer {
    fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
    }
}

impl IncidentAnalyzer for GeminiRigAnalyzer {
    fn analyze(&self, prompt: String) -> AnalysisFuture {
        let api_key = self.api_key.clone();
        let model = self.model.clone();

        Box::pin(async move {
            install_ring_crypto_provider();
            let client = gemini::Client::new(&api_key).map_err(|_| AnalysisError::Provider)?;
            let extractor = client
                .extractor::<IncidentAnalysisProposal>(&model)
                .preamble(
                    "You are a careful incident analyst. Use only the supplied records. Identify the initiating component, the earliest supported onset, and the record IDs that directly support both claims. Do not invent records or timestamps.",
                )
                .max_tokens(MAX_OUTPUT_TOKENS)
                .retries(0)
                .build();

            extractor
                .extract(prompt)
                .await
                .map_err(classify_extraction_error)
        })
    }
}

fn install_ring_crypto_provider() {
    INSTALL_RING_PROVIDER.call_once(|| {
        // Reqwest is compiled with `rustls-no-provider`; Meld explicitly chooses
        // Ring so AWS-LC and its CMake build do not enter the active graph.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn classify_extraction_error(error: ExtractionError) -> AnalysisError {
    match error {
        ExtractionError::CompletionError(_) => AnalysisError::Provider,
        ExtractionError::NoData
        | ExtractionError::DeserializationError(_)
        | ExtractionError::PromptError(_) => AnalysisError::InvalidOutput,
    }
}

/// A normal Meld worker whose execution happens through a real Rig agent.
pub struct RigWorker {
    id: WorkerId,
    provider: &'static str,
    model: String,
    request_timeout: Duration,
    analyzer: Arc<dyn IncidentAnalyzer>,
}

impl RigWorker {
    pub fn gemini(
        id: impl Into<String>,
        api_key: String,
        model: String,
        request_timeout: Duration,
    ) -> Self {
        let analyzer = Arc::new(GeminiRigAnalyzer::new(api_key, model.clone()));
        Self::with_analyzer(id, "gemini", model, request_timeout, analyzer)
    }

    pub fn with_analyzer(
        id: impl Into<String>,
        provider: &'static str,
        model: impl Into<String>,
        request_timeout: Duration,
        analyzer: Arc<dyn IncidentAnalyzer>,
    ) -> Self {
        Self {
            id: WorkerId::new(id),
            provider,
            model: model.into(),
            request_timeout,
            analyzer,
        }
    }
}

impl Worker for RigWorker {
    fn id(&self) -> WorkerId {
        self.id.clone()
    }

    fn execute(&self, request: WorkRequest) -> WorkerFuture {
        let worker_id = self.id.clone();
        let provider = self.provider;
        let model = self.model.clone();
        let request_timeout = self.request_timeout;
        let analyzer = Arc::clone(&self.analyzer);

        Box::pin(async move {
            let incident =
                request
                    .mission
                    .incident
                    .as_ref()
                    .ok_or_else(|| WorkerError::Execution {
                        message: "Rig worker requires a structured incident mission".to_owned(),
                    })?;
            let prompt = incident_prompt(&request.mission, incident);

            tracing::info!(
                event = "agent.request.started",
                worker_kind = "rig",
                %provider,
                %model,
                task_id = request.token.task_id.0,
                assignment_id = request.token.assignment_id.0,
                generation = request.token.generation.0,
                worker_id = %worker_id,
                "Rig agent request started"
            );
            let model_started = tokio::time::Instant::now();
            request
                .report_activity(WorkerActivity::AgentExecutionStarted {
                    token: request.token,
                    worker_id: worker_id.clone(),
                    provider,
                    model: model.clone(),
                })
                .await;

            let analysis =
                match tokio::time::timeout(request_timeout, analyzer.analyze(prompt)).await {
                    Ok(Ok(analysis)) => analysis,
                    Ok(Err(error)) => {
                        let error_message = error.to_string();
                        tracing::warn!(
                            event = if error == AnalysisError::InvalidOutput {
                                "agent.output.invalid"
                            } else {
                                "agent.request.failed"
                            },
                            worker_kind = "rig",
                            %provider,
                            %model,
                            task_id = request.token.task_id.0,
                            assignment_id = request.token.assignment_id.0,
                            generation = request.token.generation.0,
                            worker_id = %worker_id,
                            error_kind = %error,
                            "Rig agent request failed"
                        );
                        request
                            .report_activity(WorkerActivity::AgentExecutionFailed {
                                token: request.token,
                                worker_id: worker_id.clone(),
                                provider,
                                model: model.clone(),
                                duration_ms: elapsed_ms(model_started.elapsed()),
                                reason: error_message.clone(),
                            })
                            .await;
                        return Err(WorkerError::Execution {
                            message: error_message,
                        });
                    }
                    Err(_) => {
                        let error_message = "model provider request timed out".to_owned();
                        tracing::warn!(
                            event = "agent.request.failed",
                            worker_kind = "rig",
                            %provider,
                            %model,
                            task_id = request.token.task_id.0,
                            assignment_id = request.token.assignment_id.0,
                            generation = request.token.generation.0,
                            worker_id = %worker_id,
                            error_kind = "provider_timeout",
                            "Rig agent request timed out"
                        );
                        request
                            .report_activity(WorkerActivity::AgentExecutionFailed {
                                token: request.token,
                                worker_id: worker_id.clone(),
                                provider,
                                model: model.clone(),
                                duration_ms: elapsed_ms(model_started.elapsed()),
                                reason: error_message.clone(),
                            })
                            .await;
                        return Err(WorkerError::Execution {
                            message: error_message,
                        });
                    }
                };

            let output = analysis.into_worker_output(incident);
            let duration_ms = elapsed_ms(model_started.elapsed());

            tracing::info!(
                event = "agent.request.completed",
                worker_kind = "rig",
                %provider,
                %model,
                task_id = request.token.task_id.0,
                assignment_id = request.token.assignment_id.0,
                generation = request.token.generation.0,
                worker_id = %worker_id,
                "Rig agent produced a structured proposal"
            );
            request
                .report_activity(WorkerActivity::AgentOutputParsed {
                    token: request.token,
                    worker_id,
                    provider,
                    model,
                    duration_ms,
                    output: output.clone(),
                })
                .await;

            Ok(output)
        })
    }
}

fn elapsed_ms(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn incident_prompt(mission: &Mission, incident: &IncidentCase) -> String {
    let mut prompt = format!(
        "Mission: {}\nObjective: {}\n\nIncident records:\n",
        mission.title, mission.objective
    );
    for record in &incident.records {
        prompt.push_str(&format!(
            "- {} | {} | {} | {}\n",
            record.id, record.observed_at, record.component, record.observation
        ));
    }
    prompt.push_str(&format!(
        "\nMeld deterministic acceptance policy:\n- affected component: {}\n- earliest onset: {}\n- required evidence IDs: {}\n",
        incident.verification.expected_component,
        incident.verification.expected_onset,
        incident.verification.required_evidence_ids.join(", ")
    ));
    prompt.push_str(
        "\nReturn the initiating component, earliest supported onset timestamp, every policy-required evidence ID supported by the supplied records, and a concise evidence-grounded summary. Additional known evidence IDs are allowed.",
    );
    prompt
}

#[derive(Clone)]
pub struct RigDemoConfig {
    api_key: String,
    model: String,
    assignment_lease: Duration,
    provider_timeout: Duration,
    agent_a_delay: Duration,
}

impl RigDemoConfig {
    pub fn from_env() -> Result<Option<Self>, RigConfigError> {
        let mode = std::env::var("MELD_EXECUTION_MODE")
            .unwrap_or_else(|_| "deterministic".to_owned())
            .trim()
            .to_ascii_lowercase();
        match mode.as_str() {
            "deterministic" => return Ok(None),
            "rig" => {}
            _ => return Err(RigConfigError::InvalidExecutionMode),
        }

        let api_key = std::env::var("GEMINI_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or(RigConfigError::MissingApiKey)?;
        let model = std::env::var("MELD_GEMINI_MODEL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
        let assignment_lease =
            duration_from_env("MELD_ASSIGNMENT_LEASE_MS", DEFAULT_ASSIGNMENT_LEASE_MS)?;
        let provider_timeout =
            duration_from_env("MELD_PROVIDER_TIMEOUT_MS", DEFAULT_PROVIDER_TIMEOUT_MS)?;
        let agent_a_delay = duration_from_env("MELD_AGENT_A_DELAY_MS", DEFAULT_AGENT_A_DELAY_MS)?;

        if provider_timeout >= assignment_lease {
            return Err(RigConfigError::ProviderTimeoutMustPrecedeLease);
        }
        if agent_a_delay <= assignment_lease + provider_timeout {
            return Err(RigConfigError::AgentDelayTooShort);
        }

        Ok(Some(Self {
            api_key,
            model,
            assignment_lease,
            provider_timeout,
            agent_a_delay,
        }))
    }

    pub fn assignment_lease(&self) -> Duration {
        self.assignment_lease
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn workers(&self) -> Vec<Arc<dyn Worker>> {
        let first = RigWorker::gemini(
            "Worker A",
            self.api_key.clone(),
            self.model.clone(),
            self.provider_timeout,
        );
        let second = RigWorker::gemini(
            "Worker B",
            self.api_key.clone(),
            self.model.clone(),
            self.provider_timeout,
        );
        vec![
            Arc::new(ControlledDelayWorker::new(first, self.agent_a_delay)),
            Arc::new(second),
        ]
    }
}

fn duration_from_env(name: &'static str, default_ms: u64) -> Result<Duration, RigConfigError> {
    let milliseconds = match std::env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or(RigConfigError::InvalidDuration { name })?,
        Err(std::env::VarError::NotPresent) => default_ms,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(RigConfigError::InvalidDuration { name });
        }
    };
    Ok(Duration::from_millis(milliseconds))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RigConfigError {
    #[error("MELD_EXECUTION_MODE must be 'deterministic' or 'rig'")]
    InvalidExecutionMode,
    #[error("GEMINI_API_KEY is required when MELD_EXECUTION_MODE=rig")]
    MissingApiKey,
    #[error("{name} must be a positive whole number of milliseconds")]
    InvalidDuration { name: &'static str },
    #[error("MELD_PROVIDER_TIMEOUT_MS must be shorter than MELD_ASSIGNMENT_LEASE_MS")]
    ProviderTimeoutMustPrecedeLease,
    #[error("MELD_AGENT_A_DELAY_MS must exceed the assignment lease plus provider timeout")]
    AgentDelayTooShort,
}
