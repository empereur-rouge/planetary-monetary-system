use axum::extract::ConnectInfo;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use pms_testkit::make_test_app;
use std::net::SocketAddr;
use tower::ServiceExt; // for `oneshot`

#[tokio::test]
async fn admin_ping_requires_token() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    // Ensure the env var matches what config.dev.toml expects ("env:PMS_ADMIN_TOKEN_DEV")
    unsafe {
        std::env::set_var("PMS_ADMIN_TOKEN_DEV", "super-token-dev-123");
    }
    // Arrange
    let app = make_test_app().await?;

    // 1) sans token -> 401
    let req = Request::builder()
        .uri("/admin/ping")
        .method("GET")
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))))
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 2) avec mauvais token -> 401
    let req = Request::builder()
        .uri("/admin/ping")
        .method("GET")
        .header("Authorization", "Bearer wrong")
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))))
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 3) avec bon token -> 200
    let req = Request::builder()
        .uri("/admin/ping")
        .method("GET")
        .header("Authorization", "Bearer super-token-dev-123")
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))))
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::OK);

    Ok(())
}

#[tokio::test]
async fn timeout_layer_returns_408_on_slow_route() -> anyhow::Result<()> {
    let app = make_test_app().await?;

    // /debug/slow doit dormir > request_timeout_ms (config.dev.toml)
    let req = Request::builder()
        .uri("/debug/slow")
        .method("GET")
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))))
        .body(Body::empty())
        .unwrap();

    let res = app.oneshot(req).await?;
    assert_eq!(res.status(), StatusCode::REQUEST_TIMEOUT); // 408

    Ok(())
}

#[tokio::test]
async fn body_limit_rejects_large_submit_block() -> anyhow::Result<()> {
    let app = make_test_app().await?;

    // construit un JSON > max_body_bytes (ex: 300 Ko)
    let big_str = "X".repeat(300_000);
    let body = format!(r#"{{"dummy":"{}"}}"#, big_str);

    let req = Request::builder()
        .uri("/submit/block")
        .method("POST")
        .header("content-type", "application/json")
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1234))))
        .body(Body::from(body))
        .unwrap();

    let res = app.oneshot(req).await?;

    // Le RequestBodyLimitLayer doit répondre 413
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);

    Ok(())
}

#[tokio::test]
async fn rate_limit_returns_429_when_spammed() -> anyhow::Result<()> {
    let app = make_test_app().await?;

    // On spam /live plus que burst/rps
    let mut got_429 = false;

    // Use different ports to simulate same IP (IP based rate limit) or same connect info
    // Governor SmartIpKeyExtractor uses IP. So same IP is enough.
    let addr = SocketAddr::from(([127, 0, 0, 1], 1234));

    for _ in 0..200 {
        let req = Request::builder()
            .uri("/live")
            .method("GET")
            .extension(ConnectInfo(addr))
            .body(Body::empty())
            .unwrap();

        let res = app.clone().oneshot(req).await?;
        if res.status() == StatusCode::TOO_MANY_REQUESTS {
            got_429 = true;
            break;
        }
    }

    assert!(
        got_429,
        "On devrait obtenir au moins un 429 avec le rate-limit"
    );

    Ok(())
}
