// pms-server/src/api/middleware — Authentication and authorization middleware.

use super::state::AppState;
use crate::api_keys;
use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::middleware::Next;
use serde_json::json;
use std::net::SocketAddr;

/// Middleware to check if request is allowed for admin routes.
/// Logic:
/// 1. Allow localhost always
/// 2. If allowed_ips is configured (non-empty), check IP is in whitelist
/// 3. Require valid admin token
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
            return (StatusCode::FORBIDDEN, "IP not allowed").into_response();
        }
    }

    // 3. Require valid Admin Token
    if let Some(token) = &state.admin_token {
        if let Some(auth_header) = headers.get("Authorization") {
            if let Ok(auth_str) = auth_header.to_str() {
                if auth_str == format!("Bearer {}", token) {
                    return next.run(request).await;
                }
            }
        }
    }

    // Block otherwise
    (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
}

/// Admin-token-only middleware for per-ledger admin routes (used inside oneshot router
/// where ConnectInfo may not be available).
pub(super) async fn require_admin_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> impl IntoResponse {
    if let Some(token) = &state.admin_token {
        if let Some(auth_header) = headers.get("Authorization") {
            if let Ok(auth_str) = auth_header.to_str() {
                if auth_str == format!("Bearer {}", token) {
                    return next.run(request).await;
                }
            }
        }
    }
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
