// pms-server/src/api/middleware — Authentication, authorization, and observability middleware.

use super::state::AppState;
use crate::api_error::ApiError;
use crate::api_keys;
use axum::extract::{ConnectInfo, MatchedPath, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::middleware::Next;
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
            crate::metrics::ADMIN_AUTH_FAILURES
                .with_label_values(&["ip_not_allowed"])
                .inc();
            return ApiError::IpNotAllowed {
                ip: client_ip.to_string(),
            }
            .into_response();
        }
    }

    // 3. Require valid Admin Token — constant-time compare via helper.
    if crate::helper::is_admin_authorized(&state, &headers) {
        return next.run(request).await;
    }

    // Differentiate "no token supplied" from "wrong token" so alerts can
    // distinguish brute force from a misconfigured client. The legacy
    // `pms_admin_auth_failures_total{reason}` counter stays for
    // dashboards; the new `pms_api_errors_total{code}` is incremented
    // by the `ApiError::IntoResponse` impl with code 1001 vs 1002.
    let has_token = headers.get(axum::http::header::AUTHORIZATION).is_some()
        || headers.get("X-Admin-Token").is_some();
    let (reason_label, err) = if has_token {
        ("wrong_token", ApiError::InvalidAuth)
    } else {
        ("missing_token", ApiError::MissingAuth)
    };
    crate::metrics::ADMIN_AUTH_FAILURES
        .with_label_values(&[reason_label])
        .inc();
    err.into_response()
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
    let has_token = headers.get(axum::http::header::AUTHORIZATION).is_some()
        || headers.get("X-Admin-Token").is_some();
    let (reason_label, err) = if has_token {
        ("wrong_token", ApiError::InvalidAuth)
    } else {
        ("missing_token", ApiError::MissingAuth)
    };
    crate::metrics::ADMIN_AUTH_FAILURES
        .with_label_values(&[reason_label])
        .inc();
    err.into_response()
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
                return ApiError::InvalidField {
                    field: "X-API-Key",
                    reason: "header contains non-ASCII bytes".into(),
                }
                .into_response();
            }
        },
        None => {
            return ApiError::MissingAuth.into_response();
        }
    };

    // Vérifier la clé (constant-time comparison du hash)
    let entry = match store.verify_key(api_key) {
        Some(entry) => entry.clone(),
        None => {
            return ApiError::InvalidAuth.into_response();
        }
    };

    // Vérifier les permissions (scope vs path)
    let path = request.uri().path().to_string();
    if !api_keys::has_permission(&entry, &path) {
        return ApiError::InsufficientScope {
            required: api_keys::resolve_scope(&path).to_string(),
            granted: entry.scopes.iter().map(|s| s.to_string()).collect(),
        }
        .into_response();
    }

    // Libérer le lock avant de continuer
    drop(store);
    next.run(request).await
}

/// Middleware that rejects write requests with `503 Service Unavailable`
/// when the engine is in read-only mode (v0.7.23).
///
/// Returns `ApiError::ReadOnly { reason }` which renders to:
///
/// ```json
/// {
///   "code": 1020,
///   "message": "Service temporarily unavailable (manual): retry in 30s",
///   "error": "read_only",
///   "reason": "manual",
///   "retry_after_seconds": 30
/// }
/// ```
///
/// The `error` / `reason` / `retry_after_seconds` fields are preserved
/// for backward compatibility with v0.7.23 SDK clients; new code should
/// branch on the numeric `code` field (1020). A `Retry-After: 30` header
/// is also set so well-behaved HTTP clients back off automatically.
///
/// Apply this to the routes that produce blocks or otherwise generate
/// disk pressure (tx submit, mint, burn, faucet, fee distribution,
/// compliance freeze/seize/reverse, contract registration, gas-pool
/// deposit/withdraw, ledger create / transfer, bridge transfer).
/// **Do NOT apply** to read endpoints, recovery endpoints (compact,
/// rebuild-tips, reindex, purge, consolidate-utxos, config GET/POST,
/// api-keys CRUD) — those are how the operator gets out of read-only
/// in the first place, so blocking them defeats the purpose.
pub(super) async fn require_writable(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> impl IntoResponse {
    if state.read_only.is_armed() {
        let reason = state.read_only.reason();
        crate::metrics::READ_ONLY_REJECTIONS
            .with_label_values(&[reason.as_str()])
            .inc();
        return ApiError::ReadOnly {
            reason: reason.as_str(),
        }
        .into_response();
    }
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
