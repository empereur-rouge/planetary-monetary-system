//! Test du VRAI handler `GET /v1/version` (api_fn/version.rs::get_version).
//!
//! NB (v0.9.3) : le test unitaire de `version.rs` ne faisait que round-tripper
//! un `VersionResponse` construit à la main via serde et asserter
//! `api_version == API_VERSION` (la constante comparée à elle-même). Le handler
//! réel — qui lit `dag_version`/`schema_version` depuis le store et expose
//! `API_VERSION` — n'était jamais appelé. Ici on frappe l'endpoint via le router
//! complet et on assert les valeurs réelles.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use std::net::SocketAddr;
use tower::ServiceExt; // oneshot

#[tokio::test]
async fn get_v1_version_returns_full_version_payload() {
    let app = pms_testkit::make_test_app().await.expect("test app builds");

    let mut req = Request::builder()
        .method("GET")
        .uri("/v1/version")
        .body(Body::empty())
        .unwrap();
    // Le rate-limiter (SmartIpKeyExtractor) exige un ConnectInfo pour dériver
    // la clé par IP — sans lui, la couche renvoie 500 "Unable To Extract Key!".
    let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = app.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let raw = String::from_utf8_lossy(&bytes);
    println!("GET /v1/version → {status}\nraw body: {raw}");
    let json: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("non-JSON body ({e}): {raw}"));

    assert_eq!(status, StatusCode::OK);

    // api_version : valeur réelle exposée par le handler (pas un self-compare).
    assert_eq!(
        json["api_version"].as_u64(),
        Some(23),
        "GET /v1/version must expose API_VERSION = 23"
    );

    // software_version = CARGO_PKG_VERSION (non vide, format X.Y.Z).
    let sw = json["software_version"].as_str().expect("software_version string");
    assert!(!sw.is_empty(), "software_version must not be empty");
    assert_eq!(sw.split('.').count(), 3, "software_version must be semver X.Y.Z, got {sw}");

    // Les autres champs de version doivent être présents et bien typés.
    assert!(
        json["dag_version"].as_str().is_some(),
        "dag_version must be a string"
    );
    assert!(
        json["schema_version"].as_i64().is_some(),
        "schema_version must be an integer"
    );
    assert!(
        json["protocol_version"].as_u64().is_some(),
        "protocol_version must be an integer"
    );
}
