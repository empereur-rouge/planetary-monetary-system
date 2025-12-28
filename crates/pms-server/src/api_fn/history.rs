use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use pms_storage::{DagStorage, StoredBlock};
use pms_types_payload::PayloadEnvelope;
use pms_wallet::history::{history_page_for_address, HistoryEntry};
use crate::api::AppState;
use crate::api_fn::stream_blocks::StreamQuery;

#[derive(Deserialize)]
pub struct PageQ {
    pub after_ts: Option<i64>,
    pub after_id: Option<String>,
    pub limit:    Option<usize>,
}

#[derive(Serialize)]
pub struct PageResp<T> {
    pub items: Vec<T>,
    pub next_after_ts: Option<i64>,
    pub next_after_id: Option<String>,
    pub has_more: bool,
}

pub async fn get_encrypted_history(
    State(app): State<AppState>,
    Query(q): Query<PageQ>,
) -> Result<Json<PageResp<pms_wire::WireBlock>>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(200).min(500);

    // ⚙️ Infos réseau à partir de la config du serveur
    let nid = app._cfg.network.network_id.clone();
    let pv  = app._cfg.network.protocol_version;

    let (ids, next_cursor) = app.store
        .recent_ids_by_time(q.after_ts, q.after_id.clone(), limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // ⬇️ Ici `stored` est un Vec<StoredBlock>
    let stored = app.store
        .get_blocks_by_ids(&ids)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // mapping StoredBlock -> WireBlock avec network/proto
    let mut blocks: Vec<pms_wire::WireBlock> =
        stored
            .into_iter()
            .map(|sb| pms_wire::WireBlock {
                id: sb.id,
                parents: sb.parents,
                payload_json: sb.payload_json,
                nonce: sb.nonce,
                network_id: nid.clone(),
                protocol_version: pv as u16,
                // pour l’instant, historique sans signature
                signer_pk_hex: String::new(),
                signature_hex: String::new(),
            })
            .collect();

    // ne garder que les Encrypted
    blocks.retain(|wb| wb.payload_json.as_ref()
        .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
        .map(|env| matches!(env, PayloadEnvelope::Encrypted(_)))
        .unwrap_or(false)
    );

    let (next_after_ts, next_after_id, has_more) =
        next_cursor
            .map(|(ts, id, more)| (Some(ts), Some(id), more))
            .unwrap_or((None, None, false));

    Ok(Json(PageResp {
        items: blocks,
        next_after_ts,
        next_after_id,
        has_more,
    }))
}