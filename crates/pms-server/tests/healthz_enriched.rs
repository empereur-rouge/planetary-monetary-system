//! Integration test for the enriched `/healthz` endpoint (audit
//! finding H-healthz, v0.7.4). The pre-0.7.4 endpoint just returned a
//! flag flipped at boot — it told monitoring "the server has started"
//! but said nothing about whether the persist pipeline still worked.
//!
//! These tests fire `GET /healthz` against the in-process testkit
//! router and assert the JSON shape contains the four checks an
//! operator can act on (`rocksdb_writable`, `persist_queue_depth`,
//! `last_block_age`, `disk_free_percent`), plus the global `status`
//! field that drives the HTTP code.
//!
//! The testkit boots a fresh tempdir RocksDB and a CoreAdapter, so all
//! checks run for real — the disk-free check probes the actual
//! filesystem the temp dir lives on; the persist_queue_depth pulls
//! from the live tokio mpsc; rocksdb_writable hits the real handle.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use std::net::SocketAddr;
use tower::ServiceExt;

/// The router applies a rate-limit layer that needs a remote address;
/// `tower::ServiceExt::oneshot` doesn't provide one by default. Inject
/// a synthetic `ConnectInfo` via the request extensions, identical to
/// the trick `admin_auth_enforcement.rs` uses.
fn with_remote(mut req: Request<Body>) -> Request<Body> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

#[tokio::test]
async fn healthz_returns_structured_json_with_four_checks() {
    let app = pms_testkit::make_test_app()
        .await
        .expect("test app builds");

    let req = with_remote(
        Request::builder()
            .method("GET")
            .uri("/healthz")
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let body_bytes = axum::body::to_bytes(resp.into_body(), 65536)
        .await
        .expect("read body");
    let body: Value = serde_json::from_slice(&body_bytes).expect("body is JSON");

    println!("/healthz → {status}");
    println!("body:\n{}", serde_json::to_string_pretty(&body).unwrap());

    // The endpoint may legitimately return 200 (every check OK) or
    // 503 (e.g. the test FS is below the disk-free threshold on a
    // crowded CI runner). Both must produce a valid response shape.
    assert!(matches!(
        status,
        StatusCode::OK | StatusCode::SERVICE_UNAVAILABLE
    ));

    // Top-level shape.
    assert!(body["status"].is_string());
    assert!(body["uptime_ready"].is_boolean());
    let checks = body["checks"]
        .as_array()
        .expect("checks must be an array");

    // Exactly the five checks the operator alerts on. `read_only_mode` a été
    // ajouté avec la safety-valve read-only (v0.7.23) — le test suivait encore
    // les 4 d'origine et était silencieusement rouge.
    let names: Vec<&str> = checks
        .iter()
        .map(|c| c["name"].as_str().unwrap_or(""))
        .collect();
    println!("check names: {names:?}");
    assert_eq!(names.len(), 5, "got {names:?}");
    assert!(names.contains(&"rocksdb_writable"));
    assert!(names.contains(&"persist_queue_depth"));
    assert!(names.contains(&"last_block_age"));
    assert!(names.contains(&"disk_free_percent"));
    assert!(names.contains(&"read_only_mode"));

    // Every check carries an explicit ok flag.
    for c in checks {
        assert!(c["ok"].is_boolean(), "check {:?} missing 'ok'", c["name"]);
        assert!(c["detail"].is_object(), "check {:?} missing 'detail'", c["name"]);
    }

    // Status string must be one of the expected enum values.
    let s = body["status"].as_str().unwrap();
    assert!(
        ["ok", "degraded", "fail", "starting"].contains(&s),
        "unexpected status string: {s}"
    );
}

#[tokio::test]
async fn healthz_status_matches_check_pass_fail_aggregate() {
    // Invariant: status == "ok" iff every check.ok == true. The handler
    // is the single source of truth for this aggregation, so we
    // re-derive it from the JSON and check the response code agrees.
    let app = pms_testkit::make_test_app().await.unwrap();
    let req = with_remote(
        Request::builder()
            .method("GET")
            .uri("/healthz")
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 65536).await.unwrap())
            .unwrap();

    if body["status"] == "starting" {
        // Test app boots `_ready = true` so we shouldn't get this, but
        // be tolerant if the testkit ever changes — `starting` should
        // come with 503.
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        return;
    }

    let all_ok = body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["ok"].as_bool().unwrap_or(false));

    println!("aggregate all_ok = {all_ok}, status = {status}, body.status = {}", body["status"]);

    if all_ok {
        assert_eq!(body["status"], "ok");
        assert_eq!(status, StatusCode::OK);
    } else {
        assert_eq!(body["status"], "degraded");
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
}

#[tokio::test]
async fn livez_remains_trivial() {
    // The point of splitting livez vs healthz: livez is the "is the
    // process answering HTTP at all" probe. It must NOT depend on the
    // checks that healthz runs, because k8s would otherwise restart
    // the pod every time a check trips.
    let app = pms_testkit::make_test_app().await.unwrap();
    let req = with_remote(
        Request::builder()
            .method("GET")
            .uri("/livez")
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    println!("/livez → {}", resp.status());
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    assert_eq!(&*body, b"ok");
}
