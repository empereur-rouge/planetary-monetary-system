//! Tests de l'ALLOWLIST IP du middleware admin RÉEL (`require_local_or_admin`
//! dans `api/middleware.rs`), pilotée via le router complet de `pms_testkit`.
//!
//! NB (v0.9.3) : l'ancienne version de ce fichier définissait sa PROPRE fonction
//! `is_ip_allowed` + un `test_admin_middleware` qui « mimic » le vrai middleware,
//! et n'exerçait jamais `require_local_or_admin`. Une régression d'ordre (token
//! vérifié avant l'IP) ou un changement de code d'erreur passait inaperçu. On
//! teste désormais le middleware de prod via `tower::oneshot` + un `ConnectInfo`
//! injecté, et on assert le **code d'erreur** (donc *pourquoi* la requête est
//! rejetée, pas seulement le statut).
//!
//! Branches couvertes de `require_local_or_admin` :
//! 1. loopback → bypass total (IP + token) ;
//! 2. allowlist non vide + IP hors plage → 403 `IpNotAllowed` (code 1030), AVANT le token ;
//! 3. IP dans la plage + pas de token → 401 `MissingAuth` (code 1001) ;
//! 4. IP dans la plage + bon token → 200 (handler atteint).

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use std::net::SocketAddr;
use tower::ServiceExt; // for `oneshot`

const ADMIN_TOKEN: &str = "test-admin-token-ip-allowlist";
const ALLOWED: &[&str] = &["10.0.0.0/8"];

/// Construit une requête GET /admin/ping avec une IP source et un token optionnels.
fn admin_ping_req(ip: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri("/admin/ping");
    if let Some(t) = token {
        builder = builder.header("Authorization", format!("Bearer {t}"));
    }
    let mut req = builder.body(Body::empty()).unwrap();
    let addr: SocketAddr = format!("{ip}:9999").parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    req
}

/// Extrait le code d'erreur numérique stable du corps `{"code":NNNN,...}`.
async fn error_code(resp: axum::response::Response) -> Option<i64> {
    let bytes = axum::body::to_bytes(resp.into_body(), 8192).await.ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("code").and_then(|c| c.as_i64())
}

async fn app() -> axum::Router {
    pms_testkit::make_test_app_with_ip_allowlist(Some(ADMIN_TOKEN.to_string()), ALLOWED)
        .await
        .expect("test app builds")
}

#[tokio::test]
async fn out_of_allowlist_ip_rejected_403_code_1030() {
    let resp = app()
        .await
        .oneshot(admin_ping_req("8.8.8.8", None))
        .await
        .unwrap();
    let status = resp.status();
    let code = error_code(resp).await;
    println!("8.8.8.8 (out of 10/8), no token → {status} code={code:?}");
    assert_eq!(status, StatusCode::FORBIDDEN, "out-of-allowlist IP must be 403");
    assert_eq!(code, Some(1030), "must be IpNotAllowed (1030), not a token error");
}

#[tokio::test]
async fn out_of_allowlist_ip_rejected_even_with_valid_token() {
    // Ordre de vérification CRITIQUE : l'IP est filtrée AVANT le token. Une clé
    // volée depuis une IP non autorisée doit échouer en 403, pas passer.
    let resp = app()
        .await
        .oneshot(admin_ping_req("8.8.8.8", Some(ADMIN_TOKEN)))
        .await
        .unwrap();
    let status = resp.status();
    let code = error_code(resp).await;
    println!("8.8.8.8 + VALID token → {status} code={code:?}");
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "valid token from a disallowed IP must STILL be rejected (IP checked first)"
    );
    assert_eq!(code, Some(1030), "rejection reason must be IP, not token");
}

#[tokio::test]
async fn in_allowlist_ip_without_token_rejected_401() {
    let resp = app()
        .await
        .oneshot(admin_ping_req("10.0.0.5", None))
        .await
        .unwrap();
    let status = resp.status();
    let code = error_code(resp).await;
    println!("10.0.0.5 (in 10/8), no token → {status} code={code:?}");
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "in-allowlist IP passes the IP gate, then fails the token gate → 401"
    );
    assert_eq!(code, Some(1001), "must be MissingAuth (1001) — IP gate passed");
}

#[tokio::test]
async fn in_allowlist_ip_with_valid_token_reaches_handler() {
    let resp = app()
        .await
        .oneshot(admin_ping_req("10.0.0.5", Some(ADMIN_TOKEN)))
        .await
        .unwrap();
    let status = resp.status();
    println!("10.0.0.5 + valid token → {status}");
    assert_eq!(
        status,
        StatusCode::OK,
        "in-allowlist IP + valid token must reach the handler"
    );
}

#[tokio::test]
async fn loopback_bypasses_allowlist_with_valid_token() {
    // 127.0.0.1 n'est PAS dans 10.0.0.0/8 : si le bypass loopback n'existait pas,
    // ce serait un 403. Un 200 prouve que le loopback court-circuite l'allowlist.
    let resp = app()
        .await
        .oneshot(admin_ping_req("127.0.0.1", Some(ADMIN_TOKEN)))
        .await
        .unwrap();
    let status = resp.status();
    println!("127.0.0.1 (loopback, not in 10/8) + token → {status}");
    assert_eq!(
        status,
        StatusCode::OK,
        "loopback must bypass the IP allowlist entirely"
    );
}

#[tokio::test]
async fn cidr_boundary_matching_through_real_middleware() {
    let app = app().await;

    // Dernière IP de 10.0.0.0/8 → DANS la plage → passe l'IP gate (401 token).
    let in_edge = app
        .clone()
        .oneshot(admin_ping_req("10.255.255.255", None))
        .await
        .unwrap();
    let in_status = in_edge.status();
    let in_code = error_code(in_edge).await;
    println!("10.255.255.255 (edge in 10/8) → {in_status} code={in_code:?}");
    assert_eq!(in_status, StatusCode::UNAUTHORIZED, "10.255.255.255 is inside 10/8");
    assert_eq!(in_code, Some(1001));

    // Première IP hors 10.0.0.0/8 → HORS plage → 403 IpNotAllowed.
    let out_edge = app
        .oneshot(admin_ping_req("11.0.0.0", None))
        .await
        .unwrap();
    let out_status = out_edge.status();
    let out_code = error_code(out_edge).await;
    println!("11.0.0.0 (just outside 10/8) → {out_status} code={out_code:?}");
    assert_eq!(out_status, StatusCode::FORBIDDEN, "11.0.0.0 is outside 10/8");
    assert_eq!(out_code, Some(1030));
}
