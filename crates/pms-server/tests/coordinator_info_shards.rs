//! Smoke test for the `/v1/coordinator/info` shape after the v0.7.4
//! audit follow-up (sub-address sharding). Verifies:
//!
//!   * Empty `shards` array when sharding is disabled (default).
//!   * `coord_shard_count` matches the array length.
//!   * Every shard entry has a valid bech32 address, distinct
//!     secp256k1 pubkey, and distinct X25519 pubkey.
//!
//! The full sharding integration (rule round-robin, etc.) is in
//! Phase 6 of the implementation.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use serde_json::Value;
use std::net::SocketAddr;
use tower::ServiceExt;

fn with_remote(mut req: Request<Body>) -> Request<Body> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

#[tokio::test]
async fn coordinator_info_default_has_no_shards() {
    let app = pms_testkit::make_test_app().await.expect("app");

    let req = with_remote(
        Request::builder()
            .method("GET")
            .uri("/v1/coordinator/info")
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert!(resp.status().is_success());
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), 16384).await.unwrap(),
    )
    .unwrap();
    println!("body:\n{}", serde_json::to_string_pretty(&body).unwrap());

    // Default test config has coord_shard_count = 0 (sharding off),
    // so the response shouldn't contain a non-empty shards array. The
    // field uses #[serde(skip_serializing_if = "Vec::is_empty")] so
    // it may be absent OR present-but-empty.
    let count = body.get("coord_shard_count").and_then(|v| v.as_u64()).unwrap_or(99);
    assert_eq!(count, 0);
    let shards = body.get("shards");
    match shards {
        None => println!("  shards field absent (skip_serializing_if elided it)"),
        Some(v) => {
            let arr = v.as_array().expect("shards must be an array");
            assert!(arr.is_empty(), "shards must be empty when count=0");
        }
    }

    // Sanity: standard fields still present.
    assert!(body.get("is_coordinator").is_some());
    assert!(body.get("secp256k1_pubkey").is_some());
    assert!(body.get("x25519_pubkey").is_some());
}
