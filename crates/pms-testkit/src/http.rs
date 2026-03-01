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
