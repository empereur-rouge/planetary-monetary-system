use crate::api::AppState;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use http::StatusCode;
use pms_storage::{DagStorage, PutResult};
use pms_wire::WireBlock;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct InternalHealthResp {
    status: String,
    block_count: usize,
}

pub async fn internal_health(State(app): State<AppState>) -> impl IntoResponse {
    let count = app
        .store
        .all_block_ids()
        .await
        .map(|v| v.len())
        .unwrap_or(0);
    Json(InternalHealthResp {
        status: "ok".into(),
        block_count: count,
    })
}

#[derive(Serialize)]
pub struct TipsResp {
    tips: Vec<String>,
}

pub async fn internal_tips(State(app): State<AppState>) -> impl IntoResponse {
    let tips = app.store.top_tips(64).await.unwrap_or_default();
    Json(TipsResp { tips })
}

#[derive(Serialize)]
pub struct UtxoItem {
    txid: String,
    index: u32,
    amount: String,
    address: String,
}

#[derive(Serialize)]
pub struct UtxosResp {
    utxos: Vec<UtxoItem>,
}

pub async fn internal_utxos(
    State(app): State<AppState>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    use pms_wallet::utxo_store::gather_address_utxos_dec;
    let hrp = app.settings.address.hrp.clone();
    match gather_address_utxos_dec(&app.store, &hrp, &address, 2000).await {
        Ok(list) => {
            let utxos = list
                .into_iter()
                .map(|u| UtxoItem {
                    txid: u.txid,
                    index: u.index,
                    amount: u.amount.to_string(),
                    address: address.clone(),
                })
                .collect();
            (StatusCode::OK, Json(UtxosResp { utxos }))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(UtxosResp { utxos: vec![] }),
        ),
    }
}

#[derive(Serialize)]
pub struct BlockResp {
    id: String,
    parents: Vec<String>,
    payload_json: Option<String>,
    nonce: u64,
}

pub async fn internal_block(
    State(app): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match app.store.get_block(&id).await {
        Ok(Some(sb)) => (
            StatusCode::OK,
            Json(Some(BlockResp {
                id: sb.id,
                parents: sb.parents,
                payload_json: sb.payload_json,
                nonce: sb.nonce,
            })),
        ),
        _ => (StatusCode::NOT_FOUND, Json(None)),
    }
}

#[derive(Deserialize)]
pub struct SubmitBlockReq {
    block: WireBlock,
}

#[derive(Serialize)]
pub struct SubmitBlockResp {
    status: String,
    id: String,
    reason: Option<String>,
}

pub async fn internal_submit_block(
    State(app): State<AppState>,
    Json(req): Json<SubmitBlockReq>,
) -> impl IntoResponse {
    let adapter = app.srv.adapter_arc();
    match adapter.persist_block(&req.block).await {
        Ok(PutResult::Inserted) => {
            crate::metrics::BLOCKS_PERSISTED.with_label_values(&[&app.ledger_id]).inc();
            crate::metrics::PMS_BLOCKS_TOTAL.with_label_values(&[&app.ledger_id]).inc();
            app.srv.enqueue_broadcast(req.block.id.clone()).await;
            (
                StatusCode::CREATED,
                Json(SubmitBlockResp {
                    status: "inserted".into(),
                    id: req.block.id,
                    reason: None,
                }),
            )
        }
        Ok(PutResult::AlreadyExists) => (
            StatusCode::CONFLICT,
            Json(SubmitBlockResp {
                status: "duplicate".into(),
                id: req.block.id,
                reason: None,
            }),
        ),
        Ok(PutResult::Rejected(reason)) => (
            StatusCode::BAD_REQUEST,
            Json(SubmitBlockResp {
                status: "rejected".into(),
                id: req.block.id,
                reason: Some(reason),
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SubmitBlockResp {
                status: "error".into(),
                id: req.block.id,
                reason: Some(e.to_string()),
            }),
        ),
    }
}

#[derive(Serialize)]
pub struct ConfigResp {
    fee_rate_bps: u32,
    base_fee: String,
    coordinator_fee_bps: u32,
    treasury_fee_bps: u32,
}

pub async fn internal_config(State(app): State<AppState>) -> impl IntoResponse {
    use pms_storage::ConfigStorage;
    let cfg = app
        .store
        .get_runtime_config()
        .unwrap_or_else(|_| pms_config::RuntimeConfig::default());
    Json(ConfigResp {
        fee_rate_bps: cfg.fee_rate_bps,
        base_fee: cfg.base_fee,
        coordinator_fee_bps: cfg.coordinator_fee_bps,
        treasury_fee_bps: cfg.treasury_fee_bps,
    })
}
pub async fn internal_metrics() -> impl IntoResponse {
    use prometheus::{Encoder, TextEncoder};

    let metric_families = prometheus::gather();
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();

    let output = String::from_utf8(buffer).unwrap();
    (StatusCode::OK, output)
}

pub fn build_internal_router(state: AppState) -> Router {
    Router::new()
        .route("/internal/health", get(internal_health))
        .route("/internal/tips", get(internal_tips))
        .route("/internal/utxos/{address}", get(internal_utxos))
        .route("/internal/block/{id}", get(internal_block))
        .route("/internal/submit_block", post(internal_submit_block))
        .route("/internal/metrics", get(internal_metrics))
        .route("/internal/config", get(internal_config))
        .with_state(state)
}

pub async fn serve_internal_api(addr: &str, state: AppState) -> anyhow::Result<()> {
    let app = build_internal_router(state);
    let addr: std::net::SocketAddr = addr.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("🔧 Internal API listening on {}", addr);
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
