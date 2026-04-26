//! Non-regression tests for the admin-auth perimeter (audit finding H-auth, v0.7.4).
//!
//! The admin sub-router applies `require_local_or_admin` on every route — but
//! a handler writer can also (intentionally or not) skip re-checking the
//! token inside the handler itself. 7 endpoints currently rely on the
//! middleware alone (`/admin/api-keys/*`, `/admin/contracts/*`,
//! `/admin/consolidate-utxos`). If someone ever removes the middleware layer
//! by accident (ex: router refactor), those endpoints would be exposed
//! without any auth. These tests lock the invariant: **every `/admin/*`
//! route MUST reject a request that comes from a non-loopback IP without a
//! valid token, regardless of whether the handler checks the token itself.**
//!
//! The middleware path is exercised via `tower::ServiceExt::oneshot` with a
//! synthetic `ConnectInfo` pointing at a non-loopback IP (`10.0.0.1`). That
//! skips the localhost bypass and forces the token check.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use std::net::SocketAddr;
use tower::ServiceExt; // for `oneshot`

/// Inject a synthetic `ConnectInfo` into the request extensions so both the
/// rate limiter (`SmartIpKeyExtractor`) and the admin middleware
/// (`ConnectInfo<SocketAddr>` extractor) find a remote address.
///
/// We point at a non-loopback IP (`10.0.0.1`) so `is_loopback()` in
/// `require_local_or_admin` returns false and the token check runs.
fn inject_remote_ip(mut req: Request<Body>) -> Request<Body> {
    let addr: SocketAddr = "10.0.0.1:9999".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

/// List of every admin route plus the HTTP method used to hit it.
///
/// The set is kept in sync with `build_admin_router()` in `routes.rs`. If a
/// new `/admin/*` route is added and not listed here, this test is silent
/// (false negative) — but at least the existing surface keeps its auth
/// guarantees. For the new-route case, the right answer is to append to
/// this list as part of the PR that adds it.
fn admin_routes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GET", "/admin/ping"),
        ("POST", "/admin/compact"),
        ("POST", "/admin/distribute_fees"),
        ("GET", "/admin/config"),
        ("POST", "/admin/config"),
        ("POST", "/admin/tokens/create"),
        ("POST", "/admin/tokens/mint"),
        ("GET", "/admin/ledgers"),
        ("POST", "/admin/ledgers/create"),
        ("GET", "/admin/ledgers/main"),
        ("POST", "/admin/ledgers/main/transfer-ownership"),
        ("POST", "/admin/bridge/enable"),
        ("POST", "/admin/bridge/disable"),
        ("POST", "/admin/bridge/transfer"),
        ("POST", "/admin/faucet"),
        ("POST", "/admin/compliance/freeze"),
        ("POST", "/admin/compliance/unfreeze"),
        ("POST", "/admin/compliance/seize"),
        ("POST", "/admin/compliance/reverse"),
        ("GET", "/admin/compliance/frozen"),
        ("GET", "/admin/compliance/log"),
        ("GET", "/admin/compliance/shadow_balance"),
        ("POST", "/admin/reindex-activity"),
        ("POST", "/admin/reindex-activity-items"),
        ("POST", "/admin/consolidate-utxos"),
        ("POST", "/admin/rebuild-tips"),
        ("POST", "/admin/purge-activity"),
        ("POST", "/admin/purge-compliance-log"),
        ("GET", "/admin/rocksdb-stats"),
        ("POST", "/admin/api-keys"),
        ("GET", "/admin/api-keys"),
        // DELETE /admin/api-keys/{id}
        ("DELETE", "/admin/api-keys/some-id"),
        ("POST", "/admin/contracts"),
        ("GET", "/admin/contracts"),
        ("POST", "/admin/contracts/simulate"),
        // GET/PUT /admin/contracts/{id}
        ("GET", "/admin/contracts/some-contract"),
        ("PUT", "/admin/contracts/some-contract"),
        ("POST", "/admin/contracts/some-contract/toggle"),
        ("POST", "/admin/gas-pool/deposit"),
        ("POST", "/admin/gas-pool/withdraw"),
    ]
}

/// Build a router from testkit. Admin token is whatever the test config
/// resolves to — could even be absent. Either way, a request with no
/// Authorization header must be rejected: that's the invariant we guard.
async fn make_app_with_admin_token() -> anyhow::Result<axum::Router> {
    pms_testkit::make_test_app().await
}

#[tokio::test]
async fn every_admin_route_rejects_missing_token() {
    let app = make_app_with_admin_token()
        .await
        .expect("test app builds");

    let mut failures = Vec::new();

    for (method, path) in admin_routes() {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let req = inject_remote_ip(req);

        let resp = app
            .clone()
            .oneshot(req)
            .await
            .expect("oneshot does not panic");
        let status = resp.status();
        // 401/403 both acceptable — 401 from missing token, 403 from IP
        // allowlist (none in this test), 405 from routing mismatch (should
        // not happen, but better a 405 than a 200 for the invariant we're
        // guarding).
        if !matches!(
            status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED
        ) {
            failures.push(format!("{method} {path} -> {status}"));
        }
    }

    println!("tested {} admin routes", admin_routes().len());
    if !failures.is_empty() {
        println!("\nADMIN ROUTES THAT LET THE REQUEST THROUGH WITHOUT A TOKEN:");
        for f in &failures {
            println!("  {f}");
        }
    }
    assert!(
        failures.is_empty(),
        "at least one admin route accepted a request without an admin token; \
         see the list above. Every /admin/* endpoint MUST go through the \
         require_local_or_admin middleware."
    );
}

#[tokio::test]
async fn admin_route_rejects_wrong_token() {
    // Sanity check: a syntactically valid but wrong token is still rejected.
    // This exercises the constant-time compare path (helper::is_admin_authorized)
    // — the success path returns `false`, the middleware responds 401.
    let app = make_app_with_admin_token()
        .await
        .expect("test app builds");

    let req = Request::builder()
        .method("GET")
        .uri("/admin/ping")
        .header(
            "Authorization",
            "Bearer definitely-not-the-right-admin-token",
        )
        .body(Body::empty())
        .unwrap();
    let req = inject_remote_ip(req);

    let resp = app.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 4096)
        .await
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    println!("/admin/ping with wrong token -> {status}");
    println!("  body: {body}");

    // If no admin_token is configured in test env, helper returns false for
    // any token, so we still get 401 — that's fine for this invariant.
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "wrong admin token must be rejected with 401"
    );
}
