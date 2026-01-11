// ============================================================================
// API handlers for node registry endpoints
// ============================================================================
//
// POST /v1/register - Node registers itself
// GET /v1/nodes - Get list of active nodes

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};

use crate::api::AppState;
use crate::node_registry::{NodeInfo, NodesListResponse, RegisterNodeRequest};

/// POST /v1/register
/// Called by nodes on startup to register themselves
pub async fn register_node(
    State(st): State<AppState>,
    Json(req): Json<RegisterNodeRequest>,
) -> impl IntoResponse {
    // Validate request
    if req.node_pk.is_empty() || req.api_url.is_empty() {
        return (StatusCode::BAD_REQUEST, "node_pk and api_url required").into_response();
    }

    // Register the node
    {
        let mut registry = st.node_registry.write().await;
        registry.register(req.node_pk.clone(), req.api_url.clone());
    }

    tracing::info!(
        "📝 Node registered: {} at {}",
        &req.node_pk[..20.min(req.node_pk.len())],
        req.api_url
    );

    StatusCode::OK.into_response()
}

/// GET /v1/nodes
/// Returns shuffled list of active nodes for client-side load balancing
pub async fn list_nodes(State(st): State<AppState>) -> impl IntoResponse {
    let nodes: Vec<NodeInfo> = {
        let registry = st.node_registry.read().await;
        registry.get_active_nodes()
    };

    Json(NodesListResponse { nodes })
}

/// Heartbeat endpoint for nodes to keep themselves alive
/// POST /v1/heartbeat
pub async fn node_heartbeat(
    State(st): State<AppState>,
    Json(req): Json<RegisterNodeRequest>,
) -> impl IntoResponse {
    if req.node_pk.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    {
        let mut registry = st.node_registry.write().await;
        registry.register(req.node_pk, req.api_url);
    }

    StatusCode::OK.into_response()
}
