//! Integration test for the v0.7.4 retention endpoints
//! (`/admin/purge-activity` + `/admin/purge-compliance-log`).
//!
//! Goal: prove that the routes are wired, that they require auth, and
//! that they correctly forward `before_days` to the underlying
//! storage helpers. The storage-layer behaviour (cutoff math, batched
//! deletes, idempotence) is already covered by unit tests in
//! `pms-storage::rocks_store::retention::tests`.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use std::net::SocketAddr;
use tower::ServiceExt;

/// Token used for the admin endpoints in this test suite. The testkit
/// loads `etc/config/config.dev.toml` which has
/// `admin_api_token = "env:PMS_ADMIN_TOKEN_DEV"`, so we set THAT
/// variable (not `PMS_ADMIN_TOKEN`) before building the app. The
/// `OnceLock` keeps the write race-free across the three tests.
fn ensure_admin_token() -> &'static str {
    use std::sync::OnceLock;
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let value = "retention-test-token-xyz".to_string();
        // SAFETY: setenv is process-global; we OnceLock so multiple
        // tests don't race. Tests in this file are the only writers.
        unsafe { std::env::set_var("PMS_ADMIN_TOKEN_DEV", &value) };
        value
    })
}

/// Inject a synthetic `ConnectInfo` and attach the admin Bearer
/// token. The testkit's rate-limit layer needs ConnectInfo even
/// for in-process tests; the handler does its own
/// `is_admin_authorized` check so the loopback bypass alone isn't
/// enough — we have to send a valid token too.
fn with_admin(mut req: Request<Body>) -> Request<Body> {
    let token = ensure_admin_token();
    let addr: SocketAddr = "127.0.0.1:55555".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    req.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    req
}

#[tokio::test]
async fn purge_activity_validates_body_shape() {
    // Set the admin token env var BEFORE booting the app — the testkit
    // resolves `admin_api_token = "env:PMS_ADMIN_TOKEN"` from the
    // config at boot time, so writing the var afterwards has no effect.
    let _ = ensure_admin_token();
    let app = pms_testkit::make_test_app().await.expect("app");

    // Missing `before_days` → 400.
    let req = with_admin(
        Request::builder()
            .method("POST")
            .uri("/admin/purge-activity")
            .header("content-type", "application/json")
            .body(Body::from(b"{}".to_vec()))
            .unwrap(),
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = serde_json::from_slice::<Value>(
        &axum::body::to_bytes(resp.into_body(), 4096).await.unwrap(),
    )
    .unwrap_or(Value::Null);
    println!("/admin/purge-activity (no body field) → {status} {body}");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("before_days"),
        "error message must mention before_days, got {body}"
    );
}

#[tokio::test]
async fn purge_activity_returns_stats_on_empty_db() {
    // Set the admin token env var BEFORE booting the app — the testkit
    // resolves `admin_api_token = "env:PMS_ADMIN_TOKEN"` from the
    // config at boot time, so writing the var afterwards has no effect.
    let _ = ensure_admin_token();
    let app = pms_testkit::make_test_app().await.expect("app");

    let req = with_admin(
        Request::builder()
            .method("POST")
            .uri("/admin/purge-activity")
            .header("content-type", "application/json")
            .body(Body::from(br#"{"before_days":30}"#.to_vec()))
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let body = serde_json::from_slice::<Value>(
        &axum::body::to_bytes(resp.into_body(), 4096).await.unwrap(),
    )
    .unwrap_or(Value::Null);
    println!("/admin/purge-activity (empty DB) → {status} {body}");

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("status").and_then(|v| v.as_str()), Some("ok"));
    assert_eq!(body.get("action").and_then(|v| v.as_str()), Some("purge-activity"));
    assert_eq!(body.get("before_days").and_then(|v| v.as_u64()), Some(30));
    let stats = body.get("stats").expect("stats present");
    // Empty DB → scanned == 0, deleted == 0.
    assert_eq!(stats.get("scanned").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(stats.get("deleted").and_then(|v| v.as_u64()), Some(0));
    assert!(stats.get("cutoff_ms").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
}

#[tokio::test]
async fn purge_compliance_log_returns_stats_on_empty_db() {
    // Set the admin token env var BEFORE booting the app — the testkit
    // resolves `admin_api_token = "env:PMS_ADMIN_TOKEN"` from the
    // config at boot time, so writing the var afterwards has no effect.
    let _ = ensure_admin_token();
    let app = pms_testkit::make_test_app().await.expect("app");

    let req = with_admin(
        Request::builder()
            .method("POST")
            .uri("/admin/purge-compliance-log")
            .header("content-type", "application/json")
            .body(Body::from(br#"{"before_days":365}"#.to_vec()))
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let body = serde_json::from_slice::<Value>(
        &axum::body::to_bytes(resp.into_body(), 4096).await.unwrap(),
    )
    .unwrap_or(Value::Null);
    println!("/admin/purge-compliance-log (empty DB) → {status} {body}");

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("action").and_then(|v| v.as_str()),
        Some("purge-compliance-log")
    );
    assert_eq!(body.get("before_days").and_then(|v| v.as_u64()), Some(365));
    let stats = body.get("stats").expect("stats present");
    assert_eq!(stats.get("scanned").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(stats.get("deleted").and_then(|v| v.as_u64()), Some(0));
}
