use crate::api::AppState;
use axum::extract::State;
use axum::{Json, http::StatusCode};
use pms_storage::DagStorage;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Debug)]
pub struct GetTipsReq {
    #[serde(default)]
    pub limit: usize,
}

/// Retrieves the top tips of the DAG for use as parents for new blocks.
/// Route: POST /v1/dag/tips
pub async fn get_tips(
    State(state): State<AppState>,
    Json(req): Json<GetTipsReq>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let limit = if req.limit == 0 { 10 } else { req.limit };

    match state.srv.adapter_arc().top_tips(limit).await {
        Ok(tips) => Ok(Json(tips)),
        Err(e) => {
            tracing::error!("Failed to get tips: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Snapshot of the DAG state for a payment-rail SaaS watcher. Replaces the
/// `get_block_height()` concept of a linear blockchain with a curseur
/// monotone (`total_blocks` + `latest_block_ts_ms`) that the SaaS can use
/// to detect activity and rattraper après un crash via `GET /v1/blocks/range`.
#[derive(Serialize, Debug)]
pub struct DagStatusResponse {
    /// Approximate number of current tips (frontier of the DAG).
    pub tip_count: usize,
    /// Most recent coordinator-signed milestone block id, if any.
    pub last_milestone: Option<String>,
    /// Total number of blocks persisted (approximate, atomic counter).
    pub total_blocks: u64,
    /// Timestamp (ms since epoch) of the most recently persisted block.
    /// `None` when the DAG only contains the genesis block.
    pub latest_block_ts_ms: Option<i64>,
    /// Network identifier this engine is running on
    /// (e.g. `pms-mainnet-v1`, `pms-testnet-v1`). Lets the SaaS branch on
    /// chain context without parsing config.
    pub network_id: String,
    /// API contract version (cf. [`crate::api_fn::version::API_VERSION`]).
    /// Bump signals breaking changes; SaaS should pin a max it tested against.
    pub api_version: u32,
    /// DAG protocol version (semver). Major bump = breaking wire format
    /// (e.g. `2.0.0` introduced cross-chain replay protection).
    pub dag_version: String,
}

/// GET /v1/dag/status — single-call snapshot for SaaS watchers.
pub async fn get_dag_status(
    State(state): State<AppState>,
) -> Result<Json<DagStatusResponse>, StatusCode> {
    let adapter = state.srv.adapter_arc();
    let tip_count = adapter.tip_count_estimate().await;
    let last_milestone = adapter.last_milestone().await;

    let total_blocks = state.store.block_count_estimate().await.unwrap_or(0);
    let dag_version = state
        .store
        .get_dag_version()
        .await
        .unwrap_or_else(|_| "unknown".to_string());

    // Latest block timestamp: scan the freshest entry of `by_time` via the
    // existing `recent_ids_by_time` pagination helper. `limit=1` returns the
    // most recent block; the cursor `(ts, _id, _has_more)` carries its ts.
    let latest_block_ts_ms = state
        .store
        .recent_ids_by_time(None, None, 1)
        .await
        .ok()
        .and_then(|(_ids, cursor)| cursor.map(|(ts, _id, _more)| ts));

    Ok(Json(DagStatusResponse {
        tip_count,
        last_milestone,
        total_blocks,
        latest_block_ts_ms,
        network_id: state.settings.network.network_id.clone(),
        api_version: crate::api_fn::version::API_VERSION,
        dag_version,
    }))
}
