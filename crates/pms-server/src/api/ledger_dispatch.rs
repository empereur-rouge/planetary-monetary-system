// pms-server/src/api/ledger_dispatch — Dynamic per-ledger request routing.

use super::middleware::require_api_key;
use super::routes::{build_ledger_scoped_routes, build_ledger_admin_routes};
use super::state::AppState;
use axum::Json;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::middleware;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt as _;

/// Dynamic handler for per-ledger routes: `/l/{ledger_id}/{*rest}`
///
/// Resolves the ledger from LedgerManager at request time, builds a per-ledger
/// AppState, and forwards the request through `build_ledger_scoped_routes()`.
/// This allows dynamically created ledgers to be accessible immediately.
pub(super) async fn dynamic_ledger_handler(
    State(state): State<AppState>,
    axum::extract::Path((ledger_id, rest)): axum::extract::Path<(String, String)>,
    req: Request,
) -> Response {
    let mgr = match &state.ledger_mgr {
        Some(m) => m,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "multi-ledger not enabled"})),
            )
                .into_response();
        }
    };
    let instance = match mgr.get(&ledger_id) {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("ledger '{}' not found", ledger_id)})),
            )
                .into_response();
        }
    };

    // Build per-ledger AppState
    let mut ledger_state = state.clone();
    ledger_state.srv = crate::Server::api_only(
        instance.adapter.clone(),
        &instance.def.network_id,
        instance.def.protocol_version,
        state.node_wallet.clone(),
        Some(state.srv.broadcast_sender()),
    );
    ledger_state.store = instance.store.clone();
    ledger_state.ledger_id = ledger_id.clone();
    ledger_state.fee_pool = state.fee_pool_registry.get_or_create(&ledger_id);
    ledger_state.effective_fees = Arc::new(crate::api_fn::tx_helpers::resolve_effective_fees(
        &state.settings.fees,
        instance.def.fees.as_ref(),
    ));

    // Build a router with ledger-scoped routes + per-ledger admin routes
    // **Security fix**: Apply require_api_key to auth routes on custom ledgers
    // (was missing — auth routes were previously unprotected on per-ledger handler)
    let (public_routes, auth_routes) = build_ledger_scoped_routes();
    let auth_routes = auth_routes.route_layer(middleware::from_fn_with_state(
        ledger_state.clone(),
        require_api_key,
    ));
    let router = public_routes
        .merge(auth_routes)
        .with_state(ledger_state.clone())
        .merge(build_ledger_admin_routes(ledger_state));

    // Reconstruct request with stripped path (remove /l/{ledger_id} prefix).
    // CRITICAL: Reset extensions to avoid leaking outer path parameters
    // (ledger_id, rest) into the inner router. Without this, handlers like
    // `get_utxos_by_address(Path(address))` see 3 params instead of 1 → 500.
    let (mut parts, body) = req.into_parts();
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();
    let new_uri = format!("/{}{}", rest, query);
    parts.uri = new_uri
        .parse()
        .unwrap_or_else(|_| http::Uri::from_static("/"));
    parts.extensions = http::Extensions::new();
    let forwarded = Request::from_parts(parts, body);

    match router.oneshot(forwarded).await {
        Ok(response) => response,
        Err(infallible) => match infallible {},
    }
}
