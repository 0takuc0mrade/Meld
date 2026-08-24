fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    tracing::info!(event = "meld.started", "Meld deterministic core is ready");
    println!("Meld Phase 1 core. Run `cargo test --locked` to verify the reliability kernel.");
}
