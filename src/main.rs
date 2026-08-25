use std::sync::Arc;

use meld::api::{ApiState, router};
#[cfg(feature = "rig-worker")]
use meld::rig_worker::RigDemoConfig;
use meld::supervisor::{AppState, Supervisor};
use meld::verifier::DeterministicVerifier;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .compact()
                .with_filter(filter_fn(|metadata| metadata.target().starts_with("meld"))),
        )
        .init();

    let supervisor = Arc::new(Supervisor::new(
        Arc::new(AppState::default()),
        Arc::new(DeterministicVerifier),
    ));
    let api_state = configure_demo(ApiState::new(supervisor))?;
    let app = router(api_state);
    let address = std::env::var("MELD_BIND").unwrap_or_else(|_| "127.0.0.1:3000".to_owned());
    let listener = tokio::net::TcpListener::bind(&address).await?;

    tracing::info!(event = "meld.started", %address, "Meld is ready");
    println!("Meld is running at http://{address}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

#[cfg(feature = "rig-worker")]
fn configure_demo(state: ApiState) -> Result<ApiState, Box<dyn std::error::Error>> {
    let config =
        RigDemoConfig::from_env().map_err(|error| std::io::Error::other(error.to_string()))?;
    match config {
        Some(config) => {
            tracing::info!(
                event = "meld.execution_mode.configured",
                execution_mode = "rig",
                provider = "openai",
                model = config.model(),
                "Meld configured real-agent mode"
            );
            Ok(state.with_rig_demo(config))
        }
        None => {
            tracing::info!(
                event = "meld.execution_mode.configured",
                execution_mode = "deterministic",
                "Meld configured deterministic fallback mode"
            );
            Ok(state)
        }
    }
}

#[cfg(not(feature = "rig-worker"))]
fn configure_demo(state: ApiState) -> Result<ApiState, Box<dyn std::error::Error>> {
    let mode = std::env::var("MELD_EXECUTION_MODE")
        .unwrap_or_else(|_| "deterministic".to_owned())
        .trim()
        .to_ascii_lowercase();
    match mode.as_str() {
        "deterministic" => Ok(state),
        "rig" => Err("real-agent mode requires a build with --features rig-worker".into()),
        _ => Err("MELD_EXECUTION_MODE must be 'deterministic' or 'rig'".into()),
    }
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to install Ctrl+C shutdown handler");
    }
}
