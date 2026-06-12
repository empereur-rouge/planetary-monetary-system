use axum::body::Body;
use http::{Request, StatusCode};
use tower::ServiceExt; // oneshot

pub async fn post_json(
    app: &axum::Router,
    path: &str,
    v: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            1234,
        ))))
        .body(Body::from(v.to_string()))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .unwrap_or(serde_json::json!({ "raw": String::from_utf8_lossy(&bytes) }));
    (status, json)
}

/// Non-loopback source IP (TEST-NET-3, RFC 5737) used to exercise admin auth
/// for real: `require_local_or_admin` short-circuits on loopback, so a
/// 127.0.0.1 caller bypasses the token gate (dev convenience). In production
/// the gateway reaches the engine over a non-loopback Docker IP, so the token
/// is required — these helpers reproduce that.
const REMOTE_TEST_IP: [u8; 4] = [203, 0, 113, 7];

async fn post_json_inner(
    app: &axum::Router,
    path: &str,
    ip: [u8; 4],
    bearer: Option<&str>,
    v: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            ip, 1234,
        ))));
    if let Some(token) = bearer {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let req = builder.body(Body::from(v.to_string())).unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .unwrap_or(serde_json::json!({ "raw": String::from_utf8_lossy(&bytes) }));
    (status, json)
}

/// POST from a non-loopback IP WITH an `Authorization: Bearer <admin_token>`
/// header — exercises admin-gated routes (`require_local_or_admin`) the way the
/// gateway does in production (token required, no loopback shortcut).
pub async fn post_json_admin(
    app: &axum::Router,
    path: &str,
    admin_token: &str,
    v: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    post_json_inner(app, path, REMOTE_TEST_IP, Some(admin_token), v).await
}

/// POST from a non-loopback IP WITHOUT any auth header — the "anonymous remote
/// caller" case used to assert that admin-gated routes reject unauthenticated
/// requests (401).
pub async fn post_json_remote(
    app: &axum::Router,
    path: &str,
    v: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    post_json_inner(app, path, REMOTE_TEST_IP, None, v).await
}

pub async fn get_json(app: &axum::Router, path: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            1234,
        ))))
        .body(Body::empty())
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .unwrap_or(serde_json::json!({ "raw": String::from_utf8_lossy(&bytes) }));
    (status, json)
}
