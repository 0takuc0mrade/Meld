use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use meld::api::{ApiState, DemoConfig, router};
use meld::events::EventKind;
use meld::supervisor::{AppState, Supervisor};
use meld::verifier::DeterministicVerifier;
use tower::ServiceExt;

fn test_state() -> ApiState {
    let supervisor = Arc::new(Supervisor::new(
        Arc::new(AppState::default()),
        Arc::new(DeterministicVerifier),
    ));
    ApiState::with_demo_config(
        supervisor,
        DemoConfig {
            lease_duration: Duration::from_millis(10),
            first_worker_delay: Duration::from_millis(30),
            second_worker_delay: Duration::from_millis(5),
        },
    )
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn start_demo(state: &ApiState) -> String {
    let response = router(state.clone())
        .oneshot(
            Request::post("/api/missions/demo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    body_text(response).await
}

async fn settle_tasks() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn health_endpoint_reports_readiness_without_environment_details() {
    let response = router(test_state())
        .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains(r#""status":"ready""#));
    assert!(body.contains(r#""service":"meld""#));
    assert!(body.contains(r#""phase":3"#));
    assert!(!body.contains("environment"));
}

#[tokio::test]
async fn starting_demo_returns_a_fresh_task_and_fetchable_snapshot() {
    let state = test_state();
    let started = start_demo(&state).await;
    assert!(started.contains(r#""task_id":1"#));
    assert!(started.contains(r#""kind":"task.created""#));

    let response = router(state)
        .oneshot(Request::get("/api/tasks/1").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot = body_text(response).await;
    assert!(snapshot.contains(r#""title":"Diagnose a checkout incident""#));
    assert!(snapshot.contains(r#""task_id":1"#));
}

#[tokio::test]
async fn invalid_and_unknown_tasks_return_typed_safe_errors() {
    let app = router(test_state());
    let invalid = app
        .clone()
        .oneshot(
            Request::get("/api/tasks/not-a-number")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_text(invalid)
            .await
            .contains(r#""code":"invalid_task_id""#)
    );

    let unknown = app
        .oneshot(Request::get("/api/tasks/999").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let body = body_text(unknown).await;
    assert!(body.contains(r#""code":"task_not_found""#));
    assert!(!body.contains("SupervisorError"));
}

#[tokio::test(start_paused = true)]
async fn sse_replays_backend_events_and_honors_reconnect_cursor() {
    let state = test_state();
    start_demo(&state).await;
    settle_tasks().await;

    let snapshot = state
        .supervisor()
        .snapshot(meld::domain::TaskId(1))
        .await
        .unwrap();
    assert!(snapshot.events.len() >= 3);
    let first_sequence = snapshot.events[0].sequence;
    let expected_next = snapshot.events[1].sequence;

    let response = router(state)
        .oneshot(
            Request::get("/api/tasks/1/events")
                .header("last-event-id", first_sequence.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );

    let mut body = response.into_body();
    let frame = body.frame().await.unwrap().unwrap();
    let data = frame.into_data().unwrap();
    let payload = String::from_utf8(data.to_vec()).unwrap();
    assert!(payload.contains(&format!("id: {expected_next}")));
    assert!(!payload.contains(&format!("id: {first_sequence}\n")));
    assert!(payload.contains("event: meld"));
}

#[tokio::test(start_paused = true)]
async fn completed_snapshot_stays_authoritative_after_stale_result_becomes_api_visible() {
    let state = test_state();
    start_demo(&state).await;
    settle_tasks().await;

    tokio::time::advance(Duration::from_millis(10)).await;
    settle_tasks().await;
    tokio::time::advance(Duration::from_millis(5)).await;
    settle_tasks().await;

    let completed = state
        .supervisor()
        .snapshot(meld::domain::TaskId(1))
        .await
        .unwrap();
    assert!(matches!(
        completed.state,
        meld::domain::TaskState::Completed { .. }
    ));
    assert!(
        completed
            .events
            .iter()
            .any(|event| matches!(event.kind, EventKind::TaskCompleted { .. }))
    );
    assert!(
        !completed
            .events
            .iter()
            .any(|event| matches!(event.kind, EventKind::StaleSubmissionRejected { .. }))
    );

    tokio::time::advance(Duration::from_millis(15)).await;
    settle_tasks().await;

    let response = router(state.clone())
        .oneshot(Request::get("/api/tasks/1").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = body_text(response).await;
    assert!(body.contains(r#""status":{"name":"completed""#));
    assert!(body.contains(r#""worker_id":"Worker B""#));
    assert!(body.contains(r#""generation":2"#));
    assert!(body.contains(r#""kind":"submission.stale_rejected""#));

    let after_stale = state
        .supervisor()
        .snapshot(meld::domain::TaskId(1))
        .await
        .unwrap();
    assert!(
        after_stale
            .events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    let meld::domain::TaskState::Completed { accepted, .. } = after_stale.state else {
        panic!("stale work changed the terminal state");
    };
    assert_eq!(accepted.submission.worker_id.as_str(), "Worker B");
    assert_eq!(accepted.submission.token.generation.0, 2);
}

#[tokio::test]
async fn repeated_demo_runs_create_separate_tasks_and_histories() {
    let state = test_state();
    let first = start_demo(&state).await;
    let second = start_demo(&state).await;

    assert!(first.contains(r#""task_id":1"#));
    assert!(second.contains(r#""task_id":2"#));

    let first_snapshot = state
        .supervisor()
        .snapshot(meld::domain::TaskId(1))
        .await
        .unwrap();
    let second_snapshot = state
        .supervisor()
        .snapshot(meld::domain::TaskId(2))
        .await
        .unwrap();
    assert_ne!(first_snapshot.id, second_snapshot.id);
    assert!(
        first_snapshot
            .events
            .iter()
            .all(|event| event.task_id == first_snapshot.id)
    );
    assert!(
        second_snapshot
            .events
            .iter()
            .all(|event| event.task_id == second_snapshot.id)
    );
}

#[tokio::test]
async fn frontend_assets_are_served_locally_with_security_headers() {
    let app = router(test_state());
    let index = app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    assert!(index.headers().contains_key("content-security-policy"));
    let html = body_text(index).await;
    assert!(html.contains("Agent work,"));
    assert!(html.contains("/app.js"));
    assert!(!html.contains("https://cdn"));

    let script = app
        .oneshot(Request::get("/app.js").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(script.status(), StatusCode::OK);
    assert_eq!(
        script.headers().get("content-type").unwrap(),
        "text/javascript; charset=utf-8"
    );
}
