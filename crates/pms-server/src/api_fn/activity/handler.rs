use crate::api::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use pms_storage::DagStorage;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use serde::{Deserialize, Serialize};

use super::cache::ActivityCacheKey;
use super::classify::{classify_activity, parse_type_filter};

// ═══════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize)]
pub struct ActivityItem {
    pub block_id: String,
    pub ts_ms: i64,
    pub activity_type: String,
    pub direction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterparty: Option<String>,
    /// Ledger ID from which this activity originates.
    /// Omitted when "main" (single-ledger mode) for backward compat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Deserialize)]
pub struct ActivityQuery {
    #[serde(rename = "type")]
    pub filter_type: Option<String>,
    pub limit: Option<usize>,
    pub after_ts: Option<i64>,
    pub after_id: Option<String>,
    pub asset_id: Option<String>,
    /// Optional X25519 private key (hex) for decrypting encrypted payloads.
    /// When provided, EncryptedReward and Encrypted envelopes will be decrypted.
    pub x25519_sk_hex: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct ActivityResp {
    pub address: String,
    pub items: Vec<ActivityItem>,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_after_ts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_after_id: Option<String>,
    pub has_more: bool,
}


// ═══════════════════════════════════════════════════════════════════
// GET /v1/wallet/{address}/activity
// ═══════════════════════════════════════════════════════════════════

pub async fn get_wallet_activity(
    State(app): State<AppState>,
    Path(address): Path<String>,
    Query(q): Query<ActivityQuery>,
) -> Result<Json<ActivityResp>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(50).min(2000);
    let type_filters = parse_type_filter(&q.filter_type);

    // Cache lookup (skip for requests with decryption keys — those are user-specific)
    let cache_key = if q.x25519_sk_hex.is_none() {
        let key = ActivityCacheKey {
            address: address.clone(),
            filter_type: q.filter_type.clone(),
            asset_id: q.asset_id.clone(),
            limit,
            after_ts: q.after_ts,
            after_id: q.after_id.clone(),
        };
        if let Some(cached) = app.activity_cache.get(&key) {
            return Ok(Json(cached));
        }
        Some(key)
    } else {
        None
    };

    // Map API filter strings to storage-level category bytes for indexed lookup
    let category_bytes: Vec<u8> = type_filters
        .iter()
        .filter_map(|s| pms_storage::helpers::ActivityCategory::from_filter_type(s))
        .map(|c| c.as_byte())
        .collect::<std::collections::BTreeSet<u8>>()
        .into_iter()
        .collect();

    // Per-address index scan: directly fetches blocks involving this address
    // in reverse-chronological order.  No MAX_SCANNED needed.
    const BATCH_SIZE: usize = 500;

    let adapter = app.srv.adapter_arc();
    let ledger_tag = if app.ledger_id == "main" {
        None
    } else {
        Some(app.ledger_id.clone())
    };
    let mut items = Vec::new();
    let mut cursor_ts = q.after_ts;
    let mut cursor_id = q.after_id.clone();
    let mut last_cursor: Option<(i64, String, bool)>;

    loop {
        // Fetch entries with pre-computed items when available.
        // Each entry = (block_id, ts_ms, Option<Vec<StoredActivityItem>>).
        let (entries, next_cursor) = if !category_bytes.is_empty() {
            app.store
                .recent_activity_items_by_address_and_categories(
                    &address,
                    &category_bytes,
                    cursor_ts,
                    cursor_id.clone(),
                    BATCH_SIZE,
                )
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        } else {
            app.store
                .recent_activity_items_by_address(
                    &address,
                    cursor_ts,
                    cursor_id.clone(),
                    BATCH_SIZE,
                )
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        };

        if entries.is_empty() {
            last_cursor = None;
            break;
        }

        // Partition entries: fast path (pre-computed) vs fallback (need block fetch)
        let mut fallback_ids = Vec::new();
        for (block_id, ts, maybe_items) in &entries {
            if let Some(stored_items) = maybe_items {
                // ── Fast path: use pre-computed items directly ──
                for si in stored_items {
                    if !type_filters.is_empty()
                        && !type_filters.contains(&si.activity_type.as_str())
                    {
                        continue;
                    }
                    if let Some(ref filter_asset) = q.asset_id {
                        if si.asset_id.as_deref() != Some(filter_asset.as_str()) {
                            continue;
                        }
                    }
                    items.push(ActivityItem {
                        block_id: block_id.clone(),
                        ts_ms: *ts,
                        activity_type: si.activity_type.clone(),
                        direction: si.direction.clone(),
                        amount: si.amount.clone(),
                        asset_id: si.asset_id.clone(),
                        counterparty: si.counterparty.clone(),
                        ledger_id: ledger_tag.clone(),
                        payload: si.payload.clone(),
                    });
                }
            } else {
                // No pre-computed items — need fallback
                fallback_ids.push((block_id.clone(), *ts));
            }
        }

        // ── Fallback path: fetch full blocks, parse, and classify ──
        if !fallback_ids.is_empty() {
            let ids: Vec<String> = fallback_ids.iter().map(|(id, _)| id.clone()).collect();
            let blocks = app
                .store
                .get_blocks_by_ids(&ids)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            let ts_map: std::collections::HashMap<String, i64> =
                fallback_ids.iter().cloned().collect();

            for b in &blocks {
                let ts = ts_map.get(&b.id).copied().unwrap_or(0);
                let Some(env) = b
                    .payload_json
                    .as_ref()
                    .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
                else {
                    continue;
                };

                let plain = match env {
                    PayloadEnvelope::Plain(PlainPayload::EncryptedReward {
                        encrypted_outputs,
                        burned,
                        tx_block_id,
                    }) => match &q.x25519_sk_hex {
                        Some(sk) => {
                            match pms_wallet::history::try_decrypt_encrypted_reward(
                                &encrypted_outputs,
                                &burned,
                                &tx_block_id,
                                sk,
                                &address,
                            ) {
                                Some(decrypted) => decrypted,
                                None => continue,
                            }
                        }
                        None => continue,
                    },
                    PayloadEnvelope::Encrypted(enc) => match &q.x25519_sk_hex {
                        Some(sk) => match enc.decrypt_as_payload(sk) {
                            Ok(decrypted) => decrypted,
                            _ => {
                                if type_filters.is_empty()
                                    || type_filters.contains(&"encrypted")
                                {
                                    items.push(ActivityItem {
                                        block_id: b.id.clone(),
                                        ts_ms: ts,
                                        activity_type: "encrypted".to_string(),
                                        direction: "info".to_string(),
                                        amount: None,
                                        asset_id: None,
                                        counterparty: None,
                                        ledger_id: ledger_tag.clone(),
                                        payload: serde_json::json!({ "encrypted": true }),
                                    });
                                }
                                continue;
                            }
                        },
                        None => {
                            if type_filters.is_empty()
                                || type_filters.contains(&"encrypted")
                            {
                                items.push(ActivityItem {
                                    block_id: b.id.clone(),
                                    ts_ms: ts,
                                    activity_type: "encrypted".to_string(),
                                    direction: "info".to_string(),
                                    amount: None,
                                    asset_id: None,
                                    counterparty: None,
                                    ledger_id: ledger_tag.clone(),
                                    payload: serde_json::json!({ "encrypted": true }),
                                });
                            }
                            continue;
                        }
                    },
                    PayloadEnvelope::Plain(plain) => {
                        if !pms_wallet::history::involves_address(&plain, &address) {
                            continue;
                        }
                        plain
                    }
                };

                let classified = classify_activity(&plain, &address, &*adapter).await;
                for item in classified {
                    if !type_filters.is_empty()
                        && !type_filters.contains(&item.activity_type.as_str())
                    {
                        continue;
                    }
                    if let Some(ref filter_asset) = q.asset_id {
                        if item.asset_id.as_deref() != Some(filter_asset) {
                            continue;
                        }
                    }
                    items.push(ActivityItem {
                        block_id: b.id.clone(),
                        ts_ms: ts,
                        ledger_id: ledger_tag.clone(),
                        ..item
                    });
                }
            }
        }

        last_cursor = next_cursor;

        // Stop if we have enough items
        if items.len() > limit {
            break;
        }

        // Advance cursor for next batch
        match &last_cursor {
            Some((ts, id, _)) => {
                cursor_ts = Some(*ts);
                cursor_id = Some(id.clone());
            }
            None => break, // no more blocks
        }
    }

    // Sort newest first
    items.sort_by(|a, b| {
        b.ts_ms
            .cmp(&a.ts_ms)
            .then_with(|| b.block_id.cmp(&a.block_id))
    });
    let has_more = items.len() > limit;
    items.truncate(limit);

    let (next_after_ts, next_after_id) = if has_more {
        items
            .last()
            .map(|last| (Some(last.ts_ms), Some(last.block_id.clone())))
            .unwrap_or((None, None))
    } else {
        last_cursor
            .map(|(ts, id, _)| (Some(ts), Some(id)))
            .unwrap_or((None, None))
    };

    let count = items.len();
    let resp = ActivityResp {
        address,
        items,
        count,
        next_after_ts,
        next_after_id,
        has_more,
    };

    // Store in cache (only for non-decryption requests)
    if let Some(key) = cache_key {
        app.activity_cache.put(key, resp.clone());
    }

    Ok(Json(resp))
}
