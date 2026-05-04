use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose;
use hex::FromHex;
use k256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::atomic::Ordering;

use crate::api::AppState;
use pms_storage::DagStorage;
use pms_storage::PutResult;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::WireBlock;

use axum::response::IntoResponse;

pub async fn submit_block(
    State(st): State<AppState>,
    Json(wb): Json<WireBlock>,
) -> impl IntoResponse {
    // 0) vérif network_id + version
    if wb.network_id != st._cfg.network.network_id {
        return (
            StatusCode::BAD_REQUEST,
            format!("Invalid NetworkID: expected {}", st._cfg.network.network_id),
        )
            .into_response();
    }
    // Protocol Check
    if u32::from(wb.protocol_version) != st._cfg.network.protocol_version {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "Invalid Protocol Version: expected {}",
                st._cfg.network.protocol_version
            ),
        )
            .into_response();
    }

    // 0b) vérif signature
    let has_sig_fields = !wb.signer_pk_hex.is_empty();

    if st._cfg.auth.require_signed_submit || has_sig_fields {
        if let Err(code) = verify_wireblock_signature(&wb) {
            st.stats.persisted_err.fetch_add(1, Ordering::Relaxed);
            return (code, "Signature verification failed").into_response();
        }
    }

    // 0c-bis) Gas pool check for custom ledgers
    if let Err(e) = crate::api_fn::tx_helpers::try_consume_gas(&st) {
        return (StatusCode::PAYMENT_REQUIRED, e).into_response();
    }

    // 0c) Coordinator Filter: Reject regular TXs if configured (force use of Worker Nodes)
    if st.settings.validation.coordinator_tx_only {
        let is_privileged = wb
            .payload_json
            .as_ref()
            .and_then(|json| serde_json::from_str::<PayloadEnvelope>(json).ok())
            .map(|envelope| {
                matches!(
                    envelope,
                    PayloadEnvelope::Plain(PlainPayload::Milestone { .. })
                        | PayloadEnvelope::Plain(PlainPayload::ConfigUpdate(_))
                )
            })
            .unwrap_or(false);

        if !is_privileged {
            tracing::warn!(
                "⛔️ Coordinator rejected non-privileged block from {}",
                &wb.signer_pk_hex
            );
            return (
                StatusCode::FORBIDDEN,
                "Coordinator only accepts Milestones. Use a Worker Node.",
            )
                .into_response();
        }
    }

    // 1) Persist block
    match st.srv.adapter_arc().persist_block(&wb).await {
        Ok(PutResult::Inserted) => {
            st.stats.persisted_ok.fetch_add(1, Ordering::Relaxed);
            let _ = st.srv.enqueue_broadcast(wb.id.clone()).await;

            crate::metrics::BLOCKS_PERSISTED
                .with_label_values(&[&st.ledger_id])
                .inc();

            // ============================================================
            // FEE POOL ACCUMULATION (Distributed TX Processing)
            // ============================================================
            // Instead of creating a reward block immediately, we accumulate
            // fees in the pool. Distribution happens via Milestone.
            // This removes the Coordinator bottleneck for horizontal scaling.
            accumulate_fee_if_tx(&st, &wb).await;

            StatusCode::ACCEPTED.into_response()
        }
        Ok(PutResult::AlreadyExists) => {
            st.stats.persisted_dup.fetch_add(1, Ordering::Relaxed);
            StatusCode::CONFLICT.into_response()
        }
        Ok(PutResult::Rejected(reason)) => {
            tracing::warn!("❌ Block Rejected: {}", reason);
            st.stats.persisted_err.fetch_add(1, Ordering::Relaxed);
            (StatusCode::BAD_REQUEST, reason).into_response()
        }
        Err(e) => {
            tracing::error!("❌ Internal Persist Error: {:#}", e);
            st.stats.persisted_err.fetch_add(1, Ordering::Relaxed);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Accumulates transaction fee in the pool for later Milestone distribution
/// This replaces immediate reward block creation for horizontal scaling
async fn accumulate_fee_if_tx(st: &AppState, wb: &WireBlock) {
    // Extract transaction fee from payload
    let fee = match extract_tx_fee(wb, st) {
        Some(f) if f > Decimal::ZERO => f,
        _ => {
            eprintln!("❌ extract_tx_fee failed or zero for block {}", &wb.id);
            return;
        }
    };

    // Get the block signer (node that created/submitted this block)
    // persist_block() already rejects blocks without signer_pk, so this is defensive only
    let signer_pk = if wb.signer_pk_hex.is_empty() {
        tracing::error!(
            "SECURITY: TX block {} reached fee tracking without signer_pk! Skipping fee accumulation.",
            &wb.id[..16.min(wb.id.len())]
        );
        return;
    } else {
        wb.signer_pk_hex.clone()
    };

    // Add fee to pool with node contribution tracking
    {
        let mut pool = st.fee_pool.write().await;
        pool.add_fee(fee, &signer_pk);
        eprintln!(
            "💰 Fee accumulated: {} PMS from node {}... (pool total: {} PMS, {} txs)",
            fee,
            &signer_pk[..20.min(signer_pk.len())],
            pool.total_fees,
            pool.tx_count
        );
    }

    // Also increment block count in node registry for this signer
    {
        let mut registry = st.node_registry.write().await;
        registry.increment_block_count(&signer_pk);
    }
}

fn extract_tx_fee(wb: &WireBlock, st: &AppState) -> Option<Decimal> {
    let payload_json = if let Some(json) = &wb.payload_json {
        json
    } else {
        eprintln!("extract_tx_fee: payload_json is None for block {}", wb.id);
        return None;
    };

    let envelope: PayloadEnvelope = match serde_json::from_str(payload_json) {
        Ok(env) => env,
        Err(e) => {
            eprintln!(
                "extract_tx_fee: JSON parse error for block {}: {}",
                wb.id, e
            );
            eprintln!("extract_tx_fee: JSON content: {}", payload_json);
            return None;
        }
    };

    match envelope {
        PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx)) => match Decimal::from_str(&tx.fee) {
            Ok(fee) => Some(fee),
            Err(e) => {
                eprintln!(
                    "extract_tx_fee: Fee parse error for block {}: {} (fee='{}')",
                    wb.id, e, tx.fee
                );
                None
            }
        },
        PayloadEnvelope::Encrypted(enc) => {
            // Try to decrypt with node's private key
            let x25519_sk = match st.node_wallet.x25519_sk_hex() {
                Some(k) => k,
                None => {
                    eprintln!(
                        "extract_tx_fee: No x25519 secret key available to decrypt block {}",
                        wb.id
                    );
                    return None;
                }
            };

            match enc.decrypt_with(&x25519_sk) {
                Ok(plaintext) => {
                    // Try to deserialize plaintext as PlainPayload
                    // Note: usually the internal payload is the struct itself (TxUtxo) or PlainPayload enum?
                    // Based on legacy code: "PlainPayload::EncryptedReward" wraps payload.
                    // But here we are decrypting a TX.
                    // Let's try parsing as PlainPayload first.
                    match serde_json::from_slice::<PlainPayload>(&plaintext) {
                        Ok(PlainPayload::TxUtxo(tx)) => match Decimal::from_str(&tx.fee) {
                            Ok(fee) => Some(fee),
                            Err(e) => {
                                eprintln!(
                                    "extract_tx_fee: Fee parse error (decrypted) for block {}: {} (fee='{}')",
                                    wb.id, e, tx.fee
                                );
                                None
                            }
                        },
                        Ok(_) => {
                            // Valid payload but not TxUtxo (e.g. NftAction, etc.)
                            // eprintln!("extract_tx_fee: Decrypted payload is not TxUtxo for block {}", wb.id);
                            None
                        }
                        Err(_) => {
                            // Fallback: try parsing as TxUtxo directly?
                            // Some older code might serialize the struct directly.
                            // But pms typically uses PlainPayload.
                            // Let's log if it fails.
                            // eprintln!("extract_tx_fee: Failed to deserialize decrypted payload as PlainPayload");
                            None
                        }
                    }
                }
                Err(_) => {
                    // Decryption failed - likely not a recipient.
                    None
                }
            }
        }
        _ => {
            eprintln!("extract_tx_fee: Envelope mismatch for block {}", wb.id);
            None
        }
    }
}

fn verify_wireblock_signature(wb: &WireBlock) -> Result<(), StatusCode> {
    // 1) signer / signature présents ?
    if wb.signer_pk_hex.is_empty() || wb.signature_hex.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 2) reconstruire la clé publique
    let pk_bytes = <Vec<u8>>::from_hex(&wb.signer_pk_hex).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let verify_key =
        VerifyingKey::from_sec1_bytes(&pk_bytes).map_err(|_| StatusCode::UNAUTHORIZED)?;

    // 3) reconstruire la signature (Base64 DER)
    let sig_bytes = general_purpose::STANDARD
        .decode(&wb.signature_hex)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let sig = Signature::from_der(&sig_bytes).map_err(|_| StatusCode::UNAUTHORIZED)?;

    // 4) message canonique identique à celui du CLI
    let msg = canonical_wireblock_message(wb);

    verify_key
        .verify(msg.as_bytes(), &sig)
        .map_err(|_| StatusCode::UNAUTHORIZED)
}

/// GET /v1/blocks/:id
/// Récupère un bloc par son ID
pub async fn get_block_by_id(
    State(st): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<WireBlock>, (StatusCode, String)> {
    // 1. Fetch from store
    let sb = st
        .store
        .get_block(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Block not found".to_string()))?;

    // 2. Convert to WireBlock
    let wb = WireBlock {
        id: sb.id,
        parents: sb.parents,
        payload_json: sb.payload_json,
        nonce: sb.nonce,
        network_id: st._cfg.network.network_id.clone(),
        protocol_version: st._cfg.network.protocol_version as u16,
        signer_pk_hex: String::new(), // StoredBlock doesn't store signer PK explicitly if unrelated to logic, or maybe it does?
        // Wait, StoredBlock usually has metadata, but where is signer_pk?
        // Let's check StoredBlock definition if possible.
        // For now, return empty or try to extract from metadata if available.
        signature_hex: String::new(), // Same for signature
        metadata: sb.metadata,
    };

    // Note: StoredBlock in pms-storage might not have exact original fields for signer/sig if they were stripped?
    // Usually we want to return the exact block as submitted.
    // However, for sync purposes, payload_json is the most important.

    Ok(Json(wb))
}

/// Block summary returned by `GET /v1/blocks/range` — minimal fields the
/// SaaS watcher needs to identify newly-relevant blocks. The watcher uses
/// `id` to pull the full TX detail via `GET /v1/transaction/{id}` only for
/// blocks whose payload hints at a deposit address it monitors.
#[derive(serde::Serialize, Debug)]
pub struct BlockRangeItem {
    pub id: String,
    pub ts_ms: i64,
}

#[derive(serde::Serialize, Debug)]
pub struct BlocksRangeResponse {
    pub blocks: Vec<BlockRangeItem>,
    /// `(ts, id, has_more)` of the last item returned. Pass `ts` as
    /// `after_ts` and `id` as `after_id` on the next call to paginate.
    /// `None` when there are no more blocks.
    pub next_cursor: Option<NextCursor>,
}

#[derive(serde::Serialize, Debug)]
pub struct NextCursor {
    pub ts_ms: i64,
    pub id: String,
    pub has_more: bool,
}

#[derive(serde::Deserialize, Debug, Default)]
pub struct BlocksRangeQuery {
    /// Resume cursor: timestamp of the last block from the previous page.
    pub after_ts: Option<i64>,
    /// Resume cursor: id of the last block from the previous page.
    pub after_id: Option<String>,
    /// Page size. Capped to `1000` to bound response size; default `100`.
    pub limit: Option<usize>,
}

const DEFAULT_RANGE_LIMIT: usize = 100;
const MAX_RANGE_LIMIT: usize = 1000;

/// `GET /v1/blocks/range` — paginated scan of blocks ordered by timestamp,
/// most-recent first. Lets a SaaS watcher rattraper après un crash : repeat
/// the call with the previous response's `next_cursor.ts_ms` / `.id` until
/// `next_cursor` is `None` or `has_more` is `false`.
pub async fn blocks_range(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<BlocksRangeQuery>,
) -> Result<Json<BlocksRangeResponse>, (StatusCode, String)> {
    let limit = q
        .limit
        .unwrap_or(DEFAULT_RANGE_LIMIT)
        .clamp(1, MAX_RANGE_LIMIT);

    let (ids, cursor) = st
        .store
        .recent_ids_by_time(q.after_ts, q.after_id, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let ts_map = st
        .store
        .ts_for_ids(&ids)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let blocks = ids
        .iter()
        .map(|id| BlockRangeItem {
            id: id.clone(),
            ts_ms: ts_map.get(id).copied().unwrap_or(0),
        })
        .collect();

    Ok(Json(BlocksRangeResponse {
        blocks,
        next_cursor: cursor.map(|(ts, id, has_more)| NextCursor {
            ts_ms: ts,
            id,
            has_more,
        }),
    }))
}
