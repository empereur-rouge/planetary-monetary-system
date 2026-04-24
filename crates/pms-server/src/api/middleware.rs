// pms-server/src/api/middleware — Authentication, authorization, and observability middleware.

use super::state::AppState;
use crate::api_keys;
use axum::Json;
use axum::extract::{ConnectInfo, MatchedPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::middleware::Next;
use serde_json::json;
use std::net::SocketAddr;

/// Middleware to check if request is allowed for admin routes.
///
/// Logic, in order:
///   1. Allow localhost (loopback) without token — dev convenience.
///      SECURITY NOTE: the server MUST NOT bind directly on `0.0.0.0`;
///      production should always sit behind a trusted reverse proxy
///      (Caddy / nginx) on localhost. This is documented in the trust
///      model (see `documentation/trust-model.md`).
///   2. If `allowed_networks` is non-empty, the remote IP must be in it.
///   3. Admin token must be supplied via either `Authorization: Bearer <t>`
///      or `X-Admin-Token: <t>`. Comparison is constant-time (delegated
///      to `helper::is_admin_authorized` which uses `subtle::ConstantTimeEq`),
///      closing audit finding H-auth-A (timing leak).
pub(super) async fn require_local_or_admin(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> impl IntoResponse {
    let client_ip = addr.ip();

    // 1. Always allow localhost
    if client_ip.is_loopback() {
        return next.run(request).await;
    }

    // 2. Check IP allowlist (if configured)
    if !state.allowed_networks.is_empty() {
        let ip_allowed = state
            .allowed_networks
            .iter()
            .any(|net| net.contains(client_ip));
        if !ip_allowed {
            tracing::warn!("Admin access denied: IP {} not in allowlist", client_ip);
            crate::metrics::ADMIN_AUTH_FAILURES
                .with_label_values(&["ip_not_allowed"])
                .inc();
            return (StatusCode::FORBIDDEN, "IP not allowed").into_response();
        }
    }

    // 3. Require valid Admin Token — constant-time compare via helper.
    if crate::helper::is_admin_authorized(&state, &headers) {
        return next.run(request).await;
    }

    // Differentiate "no token supplied" from "wrong token" so alerts can
    // distinguish brute force from a misconfigured client.
    let reason = if headers.get(axum::http::header::AUTHORIZATION).is_none()
        && headers.get("X-Admin-Token").is_none()
    {
        "missing_token"
    } else {
        "wrong_token"
    };
    crate::metrics::ADMIN_AUTH_FAILURES
        .with_label_values(&[reason])
        .inc();
    (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
}

/// Admin-token-only middleware for per-ledger admin routes where `ConnectInfo`
/// is not available (nested oneshot router). Delegates to
/// `helper::is_admin_authorized` — same constant-time path as
/// `require_local_or_admin`.
pub(super) async fn require_admin_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> impl IntoResponse {
    if crate::helper::is_admin_authorized(&state, &headers) {
        return next.run(request).await;
    }
    let reason = if headers.get(axum::http::header::AUTHORIZATION).is_none()
        && headers.get("X-Admin-Token").is_none()
    {
        "missing_token"
    } else {
        "wrong_token"
    };
    crate::metrics::ADMIN_AUTH_FAILURES
        .with_label_values(&[reason])
        .inc();
    (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
}

/// Middleware pour vérifier la clé API (header `X-API-Key`) sur les routes publiques.
///
/// Comportement :
/// - Si Bearer admin token valide → passe (admin bypass)
/// - Si le store est vide → passe tout (mode dev, backward-compatible)
/// - Si X-API-Key absent → 401 "Missing API Key"
/// - Si clé invalide → 403 "Invalid API Key"
/// - Si clé révoquée → 403 "API Key revoked"
/// - Si scope insuffisant → 403 "Insufficient permissions"
pub(super) async fn require_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> impl IntoResponse {
    // Admin bypass : un admin token valide donne accès à toutes les routes
    if crate::helper::is_admin_authorized(&state, &headers) {
        return next.run(request).await;
    }

    // Lire le store (read lock — non-bloquant pour les autres lecteurs)
    let store = state.api_key_store.read().await;

    // Mode dev : si aucune clé n'est configurée, on laisse tout passer
    if store.is_empty() {
        drop(store); // Libérer le lock avant de continuer
        return next.run(request).await;
    }

    // Extraire le header X-API-Key
    let api_key = match headers.get("X-API-Key") {
        Some(value) => match value.to_str() {
            Ok(s) => s,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "Invalid X-API-Key header encoding"})),
                )
                    .into_response();
            }
        },
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": "Missing API Key",
                    "hint": "Add header X-API-Key: pk_live_... to your request"
                })),
            )
                .into_response();
        }
    };

    // Vérifier la clé (constant-time comparison du hash)
    let entry = match store.verify_key(api_key) {
        Some(entry) => entry.clone(),
        None => {
            tracing::warn!("🔑 Invalid API key attempt");
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "Invalid API Key"})),
            )
                .into_response();
        }
    };

    // Vérifier les permissions (scope vs path)
    let path = request.uri().path().to_string();
    if !api_keys::has_permission(&entry, &path) {
        tracing::warn!(
            "🔑 API key '{}' denied access to {} (scopes: {:?})",
            entry.id,
            path,
            entry.scopes
        );
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "Insufficient permissions",
                "scope_required": api_keys::resolve_scope(&path),
                "your_scopes": entry.scopes
            })),
        )
            .into_response();
    }

    // Libérer le lock avant de continuer
    drop(store);
    next.run(request).await
}

/// Middleware to record API request latency as a Prometheus histogram.
///
/// Uses `MatchedPath` to get the route template (e.g. `/v1/wallet/{addr}/balance`)
/// instead of the actual path, preventing label cardinality explosion from dynamic segments.
pub(super) async fn track_latency(
    matched_path: Option<MatchedPath>,
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let method = request.method().as_str().to_owned();
    let route = matched_path
        .map(|mp| mp.as_str().to_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    let start = std::time::Instant::now();
    let response = next.run(request).await;
    let elapsed = start.elapsed().as_secs_f64();
    crate::metrics::API_LATENCY
        .with_label_values(&[&method, &route])
        .observe(elapsed);
    response
}
