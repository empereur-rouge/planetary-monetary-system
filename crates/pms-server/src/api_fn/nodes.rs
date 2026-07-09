// ============================================================================
// API handlers for node registry endpoints
// ============================================================================
//
// POST /v1/register - Node registers itself
// GET /v1/nodes - Get list of active nodes

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};

use crate::api::AppState;
use crate::node_registry::{NodeInfo, NodesListResponse, RegisterNodeRequest};

/// Fenêtre de fraîcheur (±5 min) pour une inscription signée. Tolère la dérive
/// d'horloge entre le pair et le coordinateur ; borne l'horizon extérieur d'un
/// rejeu (la monotonie de `try_register_authenticated` ferme le rejeu exact).
const REGISTER_FRESHNESS_MS: i64 = 300_000;

/// Message canonique signé par un pair pour prouver la possession de `node_pk`
/// lors d'une inscription self-authentifiée (`/v1/register` / `/v1/heartbeat`).
/// Lie réseau + node_pk + api_url + wallet_address + ts — une signature ne peut
/// pas être détournée vers d'autres champs ni rejouée sur un autre réseau.
/// SHA-256 domain-separated, rendu en hex (comme les autres messages signés).
pub fn node_register_signing_message(
    network_id: &str,
    node_pk: &str,
    api_url: &str,
    wallet_address: Option<&str>,
    ts_ms: i64,
) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"PMS_NODE_REGISTER_v1|");
    h.update(network_id.as_bytes());
    h.update(b"|");
    h.update(node_pk.as_bytes());
    h.update(b"|");
    h.update(api_url.as_bytes());
    h.update(b"|");
    h.update(wallet_address.unwrap_or("").as_bytes());
    h.update(b"|");
    h.update(ts_ms.to_le_bytes());
    hex::encode(h.finalize())
}

/// Autorise ET applique une inscription de nœud, partagée par `/v1/register` et
/// `/v1/heartbeat`. Deux chemins :
///   1. **Opérateur** — token admin valide (ou loopback via `is_admin_authorized`).
///   2. **Pair self-authentifié** — preuve de possession de `node_pk` : signature
///      détachée sur le message canonique + fraîcheur du `ts_ms` + monotonie
///      anti-rejeu (`try_register_authenticated`).
async fn authorize_and_register(
    st: &AppState,
    headers: &HeaderMap,
    req: &RegisterNodeRequest,
) -> Result<(), (StatusCode, String)> {
    if req.node_pk.is_empty() || req.api_url.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "node_pk and api_url required".to_string(),
        ));
    }

    // Bornes de taille + format (anti-DoS). Les entrées vivent en RAM ; sans cap
    // un body ~1 MiB × MAX_REGISTERED_NODES ≈ plusieurs Go. Et un `api_url`
    // http(s) plausible évite de bourrer la liste publique `/v1/nodes` (discovery
    // client) de blobs arbitraires. S'applique aux DEUX chemins (admin inclus).
    if req.node_pk.len() > 200 {
        return Err((StatusCode::BAD_REQUEST, "node_pk too long".to_string()));
    }
    if req.api_url.len() > 512
        || !(req.api_url.starts_with("http://") || req.api_url.starts_with("https://"))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "api_url must be an http(s) URL of at most 512 bytes".to_string(),
        ));
    }
    if req.wallet_address.as_deref().map(|w| w.len()).unwrap_or(0) > 128 {
        return Err((
            StatusCode::BAD_REQUEST,
            "wallet_address too long".to_string(),
        ));
    }

    // Chemin 1 : opérateur (token admin / loopback).
    if crate::helper::is_admin_authorized(st, headers) {
        let mut registry = st.node_registry.write().await;
        registry.register(
            req.node_pk.clone(),
            req.api_url.clone(),
            req.wallet_address.clone(),
        );
        return Ok(());
    }

    // Chemin 2 : preuve de possession de node_pk (self-registration d'un pair).
    let (Some(ts_ms), Some(sig)) = (req.ts_ms, req.signature_b64.as_deref()) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "node registration requires admin auth OR a node_pk signature (ts_ms + signature_b64)"
                .to_string(),
        ));
    };

    // Fraîcheur : rejette un ts trop ancien OU trop futur. Diff en i128 pour
    // qu'un `ts_ms` malicieux (ex: i64::MIN/MAX) ne puisse pas faire déborder la
    // soustraction ni `.abs()`.
    let now = pms_utils::ts_ms() as i64;
    if (now as i128 - ts_ms as i128).abs() > REGISTER_FRESHNESS_MS as i128 {
        return Err((
            StatusCode::UNAUTHORIZED,
            "registration timestamp outside freshness window".to_string(),
        ));
    }

    // Signature (preuve de possession de la clé privée de node_pk).
    let msg = node_register_signing_message(
        &st.settings.network.network_id,
        &req.node_pk,
        &req.api_url,
        req.wallet_address.as_deref(),
        ts_ms,
    );
    if pms_core::validations::signature::verify_detached_signature(
        msg.as_bytes(),
        &req.node_pk,
        sig,
    )
    .is_err()
    {
        return Err((
            StatusCode::UNAUTHORIZED,
            "invalid node_pk signature".to_string(),
        ));
    }

    // Inscription atomique : anti-rejeu monotone + cap anti-DoS.
    let mut registry = st.node_registry.write().await;
    registry
        .try_register_authenticated(
            req.node_pk.clone(),
            req.api_url.clone(),
            req.wallet_address.clone(),
            ts_ms,
        )
        .map_err(|e| {
            let code = if e.contains("full") {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::UNAUTHORIZED
            };
            (code, e)
        })
}

/// POST /v1/register
/// Un nœud s'inscrit lui-même (token admin OU signature de `node_pk`).
pub async fn register_node(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RegisterNodeRequest>,
) -> impl IntoResponse {
    match authorize_and_register(&st, &headers, &req).await {
        Ok(()) => {
            tracing::info!(
                "📝 Node registered: {} at {}",
                &req.node_pk[..20.min(req.node_pk.len())],
                req.api_url
            );
            StatusCode::OK.into_response()
        }
        Err((code, msg)) => (code, msg).into_response(),
    }
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

/// Heartbeat endpoint for nodes to keep themselves alive.
/// POST /v1/heartbeat — même autorisation que `/v1/register` (admin OU signature
/// de `node_pk`), refresh de `last_seen`.
pub async fn node_heartbeat(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RegisterNodeRequest>,
) -> impl IntoResponse {
    match authorize_and_register(&st, &headers, &req).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err((code, msg)) => (code, msg).into_response(),
    }
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
