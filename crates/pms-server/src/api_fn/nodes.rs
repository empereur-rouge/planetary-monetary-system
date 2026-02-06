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
        registry.register(
            req.node_pk.clone(),
            req.api_url.clone(),
            req.wallet_address.clone(),
        );
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
        registry.register(req.node_pk, req.api_url, req.wallet_address);
    }

    StatusCode::OK.into_response()
}

/// GET /v1/peers
/// Returns list of connected P2P peers (socket addresses)
pub async fn list_peers(State(st): State<AppState>) -> impl IntoResponse {
    let peers = st.srv.get_p2p_peers();
    Json(peers)
}

#[derive(serde::Deserialize)]
pub struct ConnectPeerRequest {
    pub addr: String,
}

/// POST /v1/peers/connect
/// Manually connect to a P2P peer
pub async fn connect_peer(
    State(st): State<AppState>,
    Json(req): Json<ConnectPeerRequest>,
) -> impl IntoResponse {
    let tls = st.settings.tls.clone();

    // We don't strictly validate SocketAddr here to allow hostnames (e.g. node1:8080)
    // The server::connect_to_peer method handles resolution
    if req.addr.is_empty() {
        return (StatusCode::BAD_REQUEST, "Address required").into_response();
    }

    let srv = st.srv.clone();
    tokio::spawn(async move {
        if let Err(e) = srv.connect_to_peer(req.addr.clone(), tls).await {
            tracing::error!("❌ Failed to connect to peer {}: {}", req.addr, e);
        }
    });

    (StatusCode::OK, "Connection initiated").into_response()
}
