use std::sync::Arc;

use meld::api::{ApiState, router};
use meld::supervisor::{AppState, Supervisor};
use meld::verifier::DeterministicVerifier;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    let supervisor = Arc::new(Supervisor::new(
        Arc::new(AppState::default()),
        Arc::new(DeterministicVerifier),
    ));
    let app = router(ApiState::new(supervisor));
    let address = std::env::var("MELD_BIND").unwrap_or_else(|_| "127.0.0.1:3000".to_owned());
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .expect("Meld could not bind its local HTTP listener");

    tracing::info!(event = "meld.started", %address, "Meld is ready");
    println!("Meld is running at http://{address}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("Meld HTTP server stopped unexpectedly");
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to install Ctrl+C shutdown handler");
    }
}
