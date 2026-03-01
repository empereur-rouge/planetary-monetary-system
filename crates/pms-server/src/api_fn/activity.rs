use crate::api::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use pms_storage::DagStorage;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

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

#[derive(Serialize)]
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

#[derive(Deserialize)]
pub struct StreamActivityQuery {
    #[serde(rename = "type")]
    pub filter_type: Option<String>,
    /// Optional X25519 private key (hex) for decrypting encrypted payloads in real-time.
    pub x25519_sk_hex: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════
// GET /v1/wallet/{address}/activity
// ═══════════════════════════════════════════════════════════════════

pub async fn get_wallet_activity(
    State(app): State<AppState>,
    Path(address): Path<String>,
    Query(q): Query<ActivityQuery>,
) -> Result<Json<ActivityResp>, (StatusCode, String)> {
    let limit = q.limit.unwrap_or(50).min(500);
    let type_filters = parse_type_filter(&q.filter_type);

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
        // When type filters are set, use the per-type index for O(matching) scan;
        // otherwise use the untyped per-address index.
        let (ids, next_cursor) = if !category_bytes.is_empty() {
            app.store
                .recent_ids_by_address_and_categories(
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
                .recent_ids_by_address(&address, cursor_ts, cursor_id.clone(), BATCH_SIZE)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        };

        if ids.is_empty() {
            last_cursor = None;
            break;
        }

        let blocks = app
            .store
            .get_blocks_by_ids(&ids)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        let id_ts = app
            .store
            .ts_for_ids(&ids)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        for b in &blocks {
            let ts = *id_ts.get(&b.id).unwrap_or(&0);
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
                    Some(sk) => match pms_wallet::history::try_decrypt_encrypted_reward(
                        &encrypted_outputs,
                        &burned,
                        &tx_block_id,
                        sk,
                        &address,
                    ) {
                        Some(decrypted) => decrypted,
                        None => continue,
                    },
                    None => continue,
                },
                PayloadEnvelope::Encrypted(enc) => match &q.x25519_sk_hex {
                    Some(sk) => match enc.decrypt_as_payload(sk) {
                        Ok(decrypted)
                            if pms_wallet::history::involves_address(&decrypted, &address) =>
                        {
                            decrypted
                        }
                        _ => continue,
                    },
                    None => continue,
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
                // Apply type filter
                if !type_filters.is_empty() && !type_filters.contains(&item.activity_type.as_str())
                {
                    continue;
                }
                // Apply asset filter
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
    Ok(Json(ActivityResp {
        address,
        items,
        count,
        next_after_ts,
        next_after_id,
        has_more,
    }))
}

// ═══════════════════════════════════════════════════════════════════
// GET /v1/wallet/{address}/activity/stream  (SSE)
// ═══════════════════════════════════════════════════════════════════

pub async fn stream_wallet_activity(
    State(app): State<AppState>,
    Path(address): Path<String>,
    Query(q): Query<StreamActivityQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
    let bus = app.srv.adapter_arc().event_bus().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "event bus not available".to_string(),
    ))?;

    let mut rx = bus.subscribe();
    let type_filters: Vec<String> = parse_type_filter(&q.filter_type)
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    let addr = address.clone();
    let sk_opt = q.x25519_sk_hex.clone();
    let ledger_tag = if app.ledger_id == "main" {
        None
    } else {
        Some(app.ledger_id.clone())
    };

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(pms_event::PmsEvent::BlockPersisted {
                    block_id,
                    ts_ms,
                    involved_addresses,
                    payload_json,
                    ..
                }) => {
                    let addr_match = involved_addresses.iter().any(|a| a.eq_ignore_ascii_case(&addr));

                    // Parse payload
                    let env = match serde_json::from_str::<PayloadEnvelope>(&payload_json) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    // If address not in involved_addresses and payload is not encrypted
                    // (or no key provided), skip
                    let is_encrypted = matches!(&env,
                        PayloadEnvelope::Encrypted(_) |
                        PayloadEnvelope::Plain(PlainPayload::EncryptedReward { .. })
                    );
                    if !addr_match && !(is_encrypted && sk_opt.is_some()) {
                        continue;
                    }

                    // Extract/decrypt plain payload
                    let plain = match env {
                        PayloadEnvelope::Plain(PlainPayload::EncryptedReward {
                            encrypted_outputs,
                            burned,
                            tx_block_id,
                        }) => match &sk_opt {
                            Some(sk) => match pms_wallet::history::try_decrypt_encrypted_reward(
                                &encrypted_outputs, &burned, &tx_block_id, sk, &addr,
                            ) {
                                Some(decrypted) => decrypted,
                                None => continue,
                            },
                            None => continue,
                        },
                        PayloadEnvelope::Encrypted(enc) => match &sk_opt {
                            Some(sk) => match enc.decrypt_as_payload(sk) {
                                Ok(decrypted) if pms_wallet::history::involves_address(&decrypted, &addr) => decrypted,
                                _ => continue,
                            },
                            None => continue,
                        },
                        PayloadEnvelope::Plain(plain) => {
                            if !pms_wallet::history::involves_address(&plain, &addr) {
                                continue;
                            }
                            plain
                        }
                    };

                    let classified = classify_activity_sync(&plain, &addr);
                    for mut item in classified {
                        if !type_filters.is_empty()
                            && !type_filters.iter().any(|f| f == &item.activity_type)
                        {
                            continue;
                        }
                        item.block_id = block_id.clone();
                        item.ts_ms = ts_ms;
                        item.ledger_id = ledger_tag.clone();
                        if let Ok(json) = serde_json::to_string(&item) {
                            yield Ok(Event::default().event("activity").data(json));
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("SSE activity stream lagged by {} events for {}", n, addr);
                    let msg = serde_json::json!({"warning": format!("lagged by {} events", n)});
                    yield Ok(Event::default().event("warning").data(msg.to_string()));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                _ => continue,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ═══════════════════════════════════════════════════════════════════
// Classification logic
// ═══════════════════════════════════════════════════════════════════

/// Classify a PlainPayload into ActivityItems for a given address.
/// Async version with UTXO lookup for sender detection.
async fn classify_activity(
    plain: &PlainPayload,
    addr: &str,
    adapter: &dyn pms_interface::NetDagAdapter,
) -> Vec<ActivityItem> {
    match plain {
        PlainPayload::Mint { outputs } => {
            let my_outputs: Vec<_> = outputs.iter().filter(|o| o.address == addr).collect();
            my_outputs
                .iter()
                .map(|o| ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "mint".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: o.asset_id.clone(),
                    counterparty: None,
                    ledger_id: None,
                    payload: serde_json::to_value(plain).unwrap_or_default(),
                })
                .collect()
        }

        PlainPayload::TxUtxo(tx) => {
            // Resolve sender from inputs via UTXO cache
            let sender_addr = resolve_sender(tx, adapter).await;
            let is_sender = sender_addr.as_deref() == Some(addr);
            let is_receiver = tx.outputs.iter().any(|o| o.address == addr);

            let payload_val = serde_json::to_value(tx).unwrap_or_default();
            let mut items = Vec::new();

            if is_sender && is_receiver {
                // Self-transfer (consolidation, change)
                let net: rust_decimal::Decimal = tx
                    .outputs
                    .iter()
                    .filter(|o| o.address == addr)
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "transfer_self".to_string(),
                    direction: "info".to_string(),
                    amount: Some(net.to_string()),
                    asset_id: tx.outputs.first().and_then(|o| o.asset_id.clone()),
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val,
                });
            } else if is_sender {
                // Sent to someone else
                let recipient = tx
                    .outputs
                    .iter()
                    .find(|o| o.address != addr)
                    .map(|o| o.address.clone());
                let sent_amount: rust_decimal::Decimal = tx
                    .outputs
                    .iter()
                    .filter(|o| o.address != addr)
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "transfer_out".to_string(),
                    direction: "out".to_string(),
                    amount: Some(sent_amount.to_string()),
                    asset_id: tx.outputs.first().and_then(|o| o.asset_id.clone()),
                    counterparty: recipient,
                    ledger_id: None,
                    payload: payload_val,
                });
            } else if is_receiver {
                // Received from someone
                let received: rust_decimal::Decimal = tx
                    .outputs
                    .iter()
                    .filter(|o| o.address == addr)
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "transfer_in".to_string(),
                    direction: "in".to_string(),
                    amount: Some(received.to_string()),
                    asset_id: tx
                        .outputs
                        .iter()
                        .find(|o| o.address == addr)
                        .and_then(|o| o.asset_id.clone()),
                    counterparty: sender_addr,
                    ledger_id: None,
                    payload: payload_val,
                });
            }
            items
        }

        PlainPayload::Reward {
            fee_outputs,
            reward_outputs,
            ..
        } => {
            let payload_val = serde_json::to_value(plain).unwrap_or_default();
            let mut items = Vec::new();
            for o in fee_outputs.iter().filter(|o| o.address == addr) {
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "fee_received".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            for o in reward_outputs.iter().filter(|o| o.address == addr) {
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "reward".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            items
        }

        PlainPayload::Nft(action) => {
            let payload_val = serde_json::to_value(action).unwrap_or_default();
            match action {
                pms_types_nft::NftAction::Mint { creator, .. } if creator == addr => {
                    vec![ActivityItem {
                        block_id: String::new(),
                        ts_ms: 0,
                        activity_type: "nft_mint".to_string(),
                        direction: "in".to_string(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        ledger_id: None,
                        payload: payload_val,
                    }]
                }
                pms_types_nft::NftAction::Transfer { from, to, .. } => {
                    if to == addr {
                        vec![ActivityItem {
                            block_id: String::new(),
                            ts_ms: 0,
                            activity_type: "nft_transfer_in".to_string(),
                            direction: "in".to_string(),
                            amount: None,
                            asset_id: None,
                            counterparty: Some(from.clone()),
                            ledger_id: None,
                            payload: payload_val,
                        }]
                    } else if from == addr {
                        vec![ActivityItem {
                            block_id: String::new(),
                            ts_ms: 0,
                            activity_type: "nft_transfer_out".to_string(),
                            direction: "out".to_string(),
                            amount: None,
                            asset_id: None,
                            counterparty: Some(to.clone()),
                            ledger_id: None,
                            payload: payload_val,
                        }]
                    } else {
                        vec![]
                    }
                }
                pms_types_nft::NftAction::Burn { burner, .. }
                | pms_types_nft::NftAction::BatchBurn { burner, .. }
                    if burner == addr =>
                {
                    vec![ActivityItem {
                        block_id: String::new(),
                        ts_ms: 0,
                        activity_type: "nft_burn".to_string(),
                        direction: "out".to_string(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        ledger_id: None,
                        payload: payload_val,
                    }]
                }
                pms_types_nft::NftAction::Use { user, .. } if user == addr => {
                    vec![ActivityItem {
                        block_id: String::new(),
                        ts_ms: 0,
                        activity_type: "nft_use".to_string(),
                        direction: "info".to_string(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        ledger_id: None,
                        payload: payload_val,
                    }]
                }
                _ => vec![],
            }
        }

        PlainPayload::TokenCreate(meta) if meta.creator == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "token_create".to_string(),
                direction: "info".to_string(),
                amount: None,
                asset_id: Some(meta.asset_id.clone()),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(meta).unwrap_or_default(),
            }]
        }

        PlainPayload::BridgeLock {
            dest_address,
            amount,
            asset_id,
            ..
        } if dest_address == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "bridge_lock_in".to_string(),
                direction: "in".to_string(),
                amount: Some(amount.clone()),
                asset_id: asset_id.clone(),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            }]
        }

        PlainPayload::BridgeMint { outputs, .. } => outputs
            .iter()
            .filter(|o| o.address == addr)
            .map(|o| ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "bridge_mint".to_string(),
                direction: "in".to_string(),
                amount: Some(o.amount.clone()),
                asset_id: o.asset_id.clone(),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            })
            .collect(),

        PlainPayload::Freeze {
            address, reason, ..
        } if address == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "freeze".to_string(),
                direction: "info".to_string(),
                amount: None,
                asset_id: None,
                counterparty: None,
                ledger_id: None,
                payload: serde_json::json!({ "reason": reason }),
            }]
        }

        PlainPayload::Unfreeze {
            address, reason, ..
        } if address == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "unfreeze".to_string(),
                direction: "info".to_string(),
                amount: None,
                asset_id: None,
                counterparty: None,
                ledger_id: None,
                payload: serde_json::json!({ "reason": reason }),
            }]
        }

        PlainPayload::Seize {
            from_address,
            outputs,
            reason,
            ..
        } => {
            let payload_val = serde_json::json!({ "reason": reason });
            let mut items = Vec::new();
            if from_address == addr {
                let total: rust_decimal::Decimal = outputs
                    .iter()
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "seized".to_string(),
                    direction: "out".to_string(),
                    amount: Some(total.to_string()),
                    asset_id: None,
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            for o in outputs.iter().filter(|o| o.address == addr) {
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "seize_received".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: Some(from_address.clone()),
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            items
        }

        PlainPayload::Reverse {
            outputs, reason, ..
        } => {
            let payload_val = serde_json::json!({ "reason": reason });
            outputs
                .iter()
                .filter(|o| o.address == addr)
                .map(|o| ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "reverse_received".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: o.asset_id.clone(),
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                })
                .collect()
        }

        _ => vec![],
    }
}

/// Sync version for SSE (no UTXO lookup, outputs-only for TxUtxo sender detection).
fn classify_activity_sync(plain: &PlainPayload, addr: &str) -> Vec<ActivityItem> {
    match plain {
        PlainPayload::Mint { outputs } => outputs
            .iter()
            .filter(|o| o.address == addr)
            .map(|o| ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "mint".to_string(),
                direction: "in".to_string(),
                amount: Some(o.amount.clone()),
                asset_id: o.asset_id.clone(),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            })
            .collect(),

        PlainPayload::TxUtxo(tx) => {
            // In SSE mode, we can't do async UTXO lookups.
            // We classify based on outputs only.
            let is_receiver = tx.outputs.iter().any(|o| o.address == addr);
            if is_receiver {
                let received: rust_decimal::Decimal = tx
                    .outputs
                    .iter()
                    .filter(|o| o.address == addr)
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                vec![ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "transfer_in".to_string(),
                    direction: "in".to_string(),
                    amount: Some(received.to_string()),
                    asset_id: tx
                        .outputs
                        .iter()
                        .find(|o| o.address == addr)
                        .and_then(|o| o.asset_id.clone()),
                    counterparty: None,
                    ledger_id: None,
                    payload: serde_json::to_value(tx).unwrap_or_default(),
                }]
            } else {
                vec![]
            }
        }

        PlainPayload::Reward {
            fee_outputs,
            reward_outputs,
            ..
        } => {
            let payload_val = serde_json::to_value(plain).unwrap_or_default();
            let mut items = Vec::new();
            for o in fee_outputs.iter().filter(|o| o.address == addr) {
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "fee_received".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            for o in reward_outputs.iter().filter(|o| o.address == addr) {
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "reward".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            items
        }

        PlainPayload::Nft(action) => {
            let payload_val = serde_json::to_value(action).unwrap_or_default();
            match action {
                pms_types_nft::NftAction::Mint { creator, .. } if creator == addr => {
                    vec![ActivityItem {
                        block_id: String::new(),
                        ts_ms: 0,
                        activity_type: "nft_mint".to_string(),
                        direction: "in".to_string(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        ledger_id: None,
                        payload: payload_val,
                    }]
                }
                pms_types_nft::NftAction::Transfer { from, to, .. } => {
                    if to == addr {
                        vec![ActivityItem {
                            block_id: String::new(),
                            ts_ms: 0,
                            activity_type: "nft_transfer_in".to_string(),
                            direction: "in".to_string(),
                            amount: None,
                            asset_id: None,
                            counterparty: Some(from.clone()),
                            ledger_id: None,
                            payload: payload_val,
                        }]
                    } else if from == addr {
                        vec![ActivityItem {
                            block_id: String::new(),
                            ts_ms: 0,
                            activity_type: "nft_transfer_out".to_string(),
                            direction: "out".to_string(),
                            amount: None,
                            asset_id: None,
                            counterparty: Some(to.clone()),
                            ledger_id: None,
                            payload: payload_val,
                        }]
                    } else {
                        vec![]
                    }
                }
                pms_types_nft::NftAction::Burn { burner, .. }
                | pms_types_nft::NftAction::BatchBurn { burner, .. }
                    if burner == addr =>
                {
                    vec![ActivityItem {
                        block_id: String::new(),
                        ts_ms: 0,
                        activity_type: "nft_burn".to_string(),
                        direction: "out".to_string(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        ledger_id: None,
                        payload: payload_val,
                    }]
                }
                pms_types_nft::NftAction::Use { user, .. } if user == addr => {
                    vec![ActivityItem {
                        block_id: String::new(),
                        ts_ms: 0,
                        activity_type: "nft_use".to_string(),
                        direction: "info".to_string(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        ledger_id: None,
                        payload: payload_val,
                    }]
                }
                _ => vec![],
            }
        }

        PlainPayload::Freeze {
            address, reason, ..
        } if address == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "freeze".to_string(),
                direction: "info".to_string(),
                amount: None,
                asset_id: None,
                counterparty: None,
                ledger_id: None,
                payload: serde_json::json!({ "reason": reason }),
            }]
        }

        PlainPayload::Unfreeze {
            address, reason, ..
        } if address == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "unfreeze".to_string(),
                direction: "info".to_string(),
                amount: None,
                asset_id: None,
                counterparty: None,
                ledger_id: None,
                payload: serde_json::json!({ "reason": reason }),
            }]
        }

        PlainPayload::Seize {
            from_address,
            outputs,
            reason,
            ..
        } => {
            let payload_val = serde_json::json!({ "reason": reason });
            let mut items = Vec::new();
            if from_address == addr {
                let total: rust_decimal::Decimal = outputs
                    .iter()
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "seized".to_string(),
                    direction: "out".to_string(),
                    amount: Some(total.to_string()),
                    asset_id: None,
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            for o in outputs.iter().filter(|o| o.address == addr) {
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "seize_received".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: Some(from_address.clone()),
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            items
        }

        PlainPayload::Reverse {
            outputs, reason, ..
        } => outputs
            .iter()
            .filter(|o| o.address == addr)
            .map(|o| ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "reverse_received".to_string(),
                direction: "in".to_string(),
                amount: Some(o.amount.clone()),
                asset_id: o.asset_id.clone(),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::json!({ "reason": reason }),
            })
            .collect(),

        PlainPayload::TokenCreate(meta) if meta.creator == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "token_create".to_string(),
                direction: "info".to_string(),
                amount: None,
                asset_id: Some(meta.asset_id.clone()),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(meta).unwrap_or_default(),
            }]
        }

        PlainPayload::BridgeLock {
            dest_address,
            amount,
            asset_id,
            ..
        } if dest_address == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "bridge_lock_in".to_string(),
                direction: "in".to_string(),
                amount: Some(amount.clone()),
                asset_id: asset_id.clone(),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            }]
        }

        PlainPayload::BridgeMint { outputs, .. } => outputs
            .iter()
            .filter(|o| o.address == addr)
            .map(|o| ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "bridge_mint".to_string(),
                direction: "in".to_string(),
                amount: Some(o.amount.clone()),
                asset_id: o.asset_id.clone(),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            })
            .collect(),

        _ => vec![],
    }
}

// ═══════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════

fn parse_type_filter(filter: &Option<String>) -> Vec<&str> {
    match filter {
        Some(s) if !s.is_empty() => s.split(',').map(|s| s.trim()).collect(),
        _ => vec![],
    }
}

/// Resolve the sender address from transaction inputs via UTXO cache.
async fn resolve_sender(
    tx: &pms_types::Transaction,
    adapter: &dyn pms_interface::NetDagAdapter,
) -> Option<String> {
    if let Some(first_input) = tx.inputs.first() {
        if let Some(utxo) = adapter.get_utxo(&first_input.out).await {
            return Some(utxo.address);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pms_types::{OutputId, Transaction, TxInput, TxOutput};
    use pms_types_nft::NftAction;
    use pms_types_payload::TokenMetadata;

    // ── helpers ──────────────────────────────────────────────────────

    fn out(addr: &str, amount: &str) -> TxOutput {
        TxOutput {
            address: addr.into(),
            amount: amount.into(),
            asset_id: None,
        }
    }

    fn out_asset(addr: &str, amount: &str, asset: &str) -> TxOutput {
        TxOutput {
            address: addr.into(),
            amount: amount.into(),
            asset_id: Some(asset.into()),
        }
    }

    // ── parse_type_filter ────────────────────────────────────────────

    #[test]
    fn parse_type_filter_none() {
        assert!(parse_type_filter(&None).is_empty());
    }

    #[test]
    fn parse_type_filter_empty() {
        assert!(parse_type_filter(&Some(String::new())).is_empty());
    }

    #[test]
    fn parse_type_filter_single() {
        let s = Some("fee_received".into());
        let f = parse_type_filter(&s);
        assert_eq!(f, vec!["fee_received"]);
    }

    #[test]
    fn parse_type_filter_multi() {
        let s = Some("fee_received, transfer_in , mint".into());
        let f = parse_type_filter(&s);
        assert_eq!(f, vec!["fee_received", "transfer_in", "mint"]);
    }

    // ── classify_activity_sync: Mint ─────────────────────────────────

    #[test]
    fn classify_mint_for_recipient() {
        let plain = PlainPayload::Mint {
            outputs: vec![out("alice", "100"), out("bob", "50")],
        };
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "mint");
        assert_eq!(items[0].direction, "in");
        assert_eq!(items[0].amount.as_deref(), Some("100"));
    }

    #[test]
    fn classify_mint_not_involved() {
        let plain = PlainPayload::Mint {
            outputs: vec![out("alice", "100")],
        };
        let items = classify_activity_sync(&plain, "carol");
        assert!(items.is_empty());
    }

    // ── classify_activity_sync: TxUtxo (outputs-only in sync) ───────

    #[test]
    fn classify_tx_receiver() {
        let tx = Transaction {
            inputs: vec![],
            outputs: vec![out("bob", "50"), out("alice", "30")],
            fee: "1".into(),
            unlocks: vec![],
        };
        let items = classify_activity_sync(&PlainPayload::TxUtxo(tx), "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "transfer_in");
        assert_eq!(items[0].direction, "in");
        assert_eq!(items[0].amount.as_deref(), Some("30"));
    }

    #[test]
    fn classify_tx_not_in_outputs() {
        let tx = Transaction {
            inputs: vec![],
            outputs: vec![out("bob", "50")],
            fee: "1".into(),
            unlocks: vec![],
        };
        // In sync mode, sender isn't resolved, so if addr not in outputs → empty
        let items = classify_activity_sync(&PlainPayload::TxUtxo(tx), "alice");
        assert!(items.is_empty());
    }

    // ── classify_activity_sync: Reward ───────────────────────────────

    #[test]
    fn classify_fee_received() {
        let plain = PlainPayload::Reward {
            fee_outputs: vec![out("coordinator", "1.95")],
            reward_outputs: vec![],
            burned: "0.05".into(),
            tx_block_id: "txblk1".into(),
        };
        let items = classify_activity_sync(&plain, "coordinator");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "fee_received");
        assert_eq!(items[0].direction, "in");
        assert_eq!(items[0].amount.as_deref(), Some("1.95"));
    }

    #[test]
    fn classify_reward_received() {
        let plain = PlainPayload::Reward {
            fee_outputs: vec![],
            reward_outputs: vec![out("treasury", "10")],
            burned: "0".into(),
            tx_block_id: "txblk2".into(),
        };
        let items = classify_activity_sync(&plain, "treasury");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "reward");
        assert_eq!(items[0].direction, "in");
        assert_eq!(items[0].amount.as_deref(), Some("10"));
    }

    #[test]
    fn classify_reward_both_fee_and_reward() {
        let plain = PlainPayload::Reward {
            fee_outputs: vec![out("addr", "5")],
            reward_outputs: vec![out("addr", "10")],
            burned: "0".into(),
            tx_block_id: "txblk3".into(),
        };
        let items = classify_activity_sync(&plain, "addr");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].activity_type, "fee_received");
        assert_eq!(items[1].activity_type, "reward");
    }

    #[test]
    fn classify_reward_not_involved() {
        let plain = PlainPayload::Reward {
            fee_outputs: vec![out("coordinator", "5")],
            reward_outputs: vec![],
            burned: "0".into(),
            tx_block_id: "txblk4".into(),
        };
        let items = classify_activity_sync(&plain, "random_addr");
        assert!(items.is_empty());
    }

    // ── classify_activity_sync: NFT ──────────────────────────────────

    #[test]
    fn classify_nft_mint() {
        let plain = PlainPayload::Nft(NftAction::Mint {
            token_id: "nft-001".into(),
            creator: "alice".into(),
            metadata: Default::default(),
        });
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "nft_mint");
        assert_eq!(items[0].direction, "in");
        assert!(items[0].amount.is_none());
    }

    #[test]
    fn classify_nft_mint_not_creator() {
        let plain = PlainPayload::Nft(NftAction::Mint {
            token_id: "nft-001".into(),
            creator: "alice".into(),
            metadata: Default::default(),
        });
        let items = classify_activity_sync(&plain, "bob");
        assert!(items.is_empty());
    }

    #[test]
    fn classify_nft_transfer_in() {
        let plain = PlainPayload::Nft(NftAction::Transfer {
            token_id: "nft-001".into(),
            from: "alice".into(),
            to: "bob".into(),
            new_owner_x25519_pubkey: None,
            encrypted_metadata: None,
        });
        let items = classify_activity_sync(&plain, "bob");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "nft_transfer_in");
        assert_eq!(items[0].direction, "in");
        assert_eq!(items[0].counterparty.as_deref(), Some("alice"));
    }

    #[test]
    fn classify_nft_transfer_out() {
        let plain = PlainPayload::Nft(NftAction::Transfer {
            token_id: "nft-001".into(),
            from: "alice".into(),
            to: "bob".into(),
            new_owner_x25519_pubkey: None,
            encrypted_metadata: None,
        });
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "nft_transfer_out");
        assert_eq!(items[0].direction, "out");
        assert_eq!(items[0].counterparty.as_deref(), Some("bob"));
    }

    #[test]
    fn classify_nft_burn() {
        let plain = PlainPayload::Nft(NftAction::Burn {
            token_id: "nft-001".into(),
            burner: "alice".into(),
        });
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "nft_burn");
        assert_eq!(items[0].direction, "out");
    }

    #[test]
    fn classify_nft_batch_burn() {
        let plain = PlainPayload::Nft(NftAction::BatchBurn {
            token_ids: vec!["nft-001".into(), "nft-002".into()],
            burner: "alice".into(),
        });
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "nft_burn");
    }

    #[test]
    fn classify_nft_use() {
        let plain = PlainPayload::Nft(NftAction::Use {
            token_id: "nft-001".into(),
            user: "alice".into(),
            action_type: "redeem".into(),
            action_data: None,
        });
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "nft_use");
        assert_eq!(items[0].direction, "info");
    }

    // ── classify_activity_sync: TokenCreate ──────────────────────────

    #[test]
    fn classify_token_create() {
        let plain = PlainPayload::TokenCreate(TokenMetadata {
            asset_id: "edenite".into(),
            symbol: "EDEN".into(),
            name: "Edenite".into(),
            decimals: 8,
            max_supply: None,
            creator: "alice".into(),
            mint_authority: "alice".into(),
        });
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "token_create");
        assert_eq!(items[0].direction, "info");
        assert_eq!(items[0].asset_id.as_deref(), Some("edenite"));
    }

    #[test]
    fn classify_token_create_not_creator() {
        let plain = PlainPayload::TokenCreate(TokenMetadata {
            asset_id: "edenite".into(),
            symbol: "EDEN".into(),
            name: "Edenite".into(),
            decimals: 8,
            max_supply: None,
            creator: "alice".into(),
            mint_authority: "alice".into(),
        });
        let items = classify_activity_sync(&plain, "bob");
        assert!(items.is_empty());
    }

    // ── classify_activity_sync: Compliance ───────────────────────────

    #[test]
    fn classify_freeze() {
        let plain = PlainPayload::Freeze {
            address: "alice".into(),
            reason: "suspicious".into(),
        };
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "freeze");
        assert_eq!(items[0].direction, "info");
    }

    #[test]
    fn classify_freeze_not_target() {
        let plain = PlainPayload::Freeze {
            address: "alice".into(),
            reason: "suspicious".into(),
        };
        let items = classify_activity_sync(&plain, "bob");
        assert!(items.is_empty());
    }

    #[test]
    fn classify_unfreeze() {
        let plain = PlainPayload::Unfreeze {
            address: "alice".into(),
            reason: "cleared".into(),
            freeze_block_id: "blk0".into(),
        };
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "unfreeze");
        assert_eq!(items[0].direction, "info");
    }

    #[test]
    fn classify_seized_from_target() {
        let plain = PlainPayload::Seize {
            from_address: "alice".into(),
            inputs: vec![],
            outputs: vec![out("treasury", "1000")],
            reason: "court order".into(),
        };
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "seized");
        assert_eq!(items[0].direction, "out");
        assert_eq!(items[0].amount.as_deref(), Some("1000"));
    }

    #[test]
    fn classify_seize_received() {
        let plain = PlainPayload::Seize {
            from_address: "alice".into(),
            inputs: vec![],
            outputs: vec![out("treasury", "1000")],
            reason: "court order".into(),
        };
        let items = classify_activity_sync(&plain, "treasury");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "seize_received");
        assert_eq!(items[0].direction, "in");
        assert_eq!(items[0].amount.as_deref(), Some("1000"));
        assert_eq!(items[0].counterparty.as_deref(), Some("alice"));
    }

    #[test]
    fn classify_reverse_received() {
        let plain = PlainPayload::Reverse {
            original_block_id: "blk1".into(),
            inputs: vec![],
            outputs: vec![out("alice", "500")],
            reason: "fraud".into(),
        };
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "reverse_received");
        assert_eq!(items[0].direction, "in");
        assert_eq!(items[0].amount.as_deref(), Some("500"));
    }

    #[test]
    fn classify_reverse_not_involved() {
        let plain = PlainPayload::Reverse {
            original_block_id: "blk1".into(),
            inputs: vec![],
            outputs: vec![out("alice", "500")],
            reason: "fraud".into(),
        };
        let items = classify_activity_sync(&plain, "bob");
        assert!(items.is_empty());
    }

    // ── classify_activity_sync: Bridge ───────────────────────────────

    #[test]
    fn classify_bridge_lock_in() {
        let plain = PlainPayload::BridgeLock {
            inputs: vec![],
            dest_address: "alice".into(),
            amount: "250".into(),
            asset_id: Some("edenite".into()),
            dest_ledger_id: "side".into(),
        };
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "bridge_lock_in");
        assert_eq!(items[0].direction, "in");
        assert_eq!(items[0].amount.as_deref(), Some("250"));
        assert_eq!(items[0].asset_id.as_deref(), Some("edenite"));
    }

    #[test]
    fn classify_bridge_lock_not_dest() {
        let plain = PlainPayload::BridgeLock {
            inputs: vec![],
            dest_address: "alice".into(),
            amount: "250".into(),
            asset_id: None,
            dest_ledger_id: "side".into(),
        };
        let items = classify_activity_sync(&plain, "bob");
        assert!(items.is_empty());
    }

    #[test]
    fn classify_bridge_mint() {
        let plain = PlainPayload::BridgeMint {
            outputs: vec![out("alice", "250")],
            lock_block_id: "lock1".into(),
            source_ledger_id: "main".into(),
        };
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "bridge_mint");
        assert_eq!(items[0].direction, "in");
        assert_eq!(items[0].amount.as_deref(), Some("250"));
    }

    // ── classify_activity_sync: irrelevant payloads ──────────────────

    #[test]
    fn classify_genesis_returns_empty() {
        let plain = PlainPayload::Genesis;
        let items = classify_activity_sync(&plain, "alice");
        assert!(items.is_empty());
    }

    // ── classify_activity (async) with mock adapter ──────────────────

    #[tokio::test]
    async fn classify_tx_transfer_in_async() {
        use async_trait::async_trait;
        use pms_interface::NetDagAdapter;

        struct MockAdapter;

        #[async_trait]
        impl NetDagAdapter for MockAdapter {
            async fn have_block(&self, _id: &str) -> bool {
                false
            }
            async fn persist_block(
                &self,
                _wb: &pms_wire::WireBlock,
            ) -> anyhow::Result<pms_storage::PutResult> {
                Ok(pms_storage::PutResult::Inserted)
            }
            async fn broadcast_block(&self, _b: &pms_wire::WireBlock) -> anyhow::Result<()> {
                Ok(())
            }
            async fn top_tips(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
                Ok(vec![])
            }
            async fn get_block(&self, _id: &str) -> anyhow::Result<Option<pms_wire::WireBlock>> {
                Ok(None)
            }
            async fn recent_ids(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
                Ok(vec![])
            }
            async fn get_blocks_by_ids(
                &self,
                _ids: &[String],
            ) -> anyhow::Result<Vec<pms_wire::WireBlock>> {
                Ok(vec![])
            }
            fn min_pow_leading_zero_bits(&self) -> u8 {
                0
            }
            async fn circulating_supply(&self) -> (rust_decimal::Decimal, u64) {
                (rust_decimal::Decimal::ZERO, 0)
            }
            async fn circulating_supply_by_asset(
                &self,
                _asset_id: Option<&str>,
            ) -> (rust_decimal::Decimal, u64) {
                (rust_decimal::Decimal::ZERO, 0)
            }
            async fn balance_by_address(&self, _address: &str) -> rust_decimal::Decimal {
                rust_decimal::Decimal::ZERO
            }
            async fn utxos_by_address(
                &self,
                _address: &str,
            ) -> Vec<(pms_types::OutputId, pms_types::TxOutput)> {
                vec![]
            }
            async fn add_utxo(
                &self,
                _txid: String,
                _index: u32,
                _address: String,
                _amount: String,
                _asset_id: Option<String>,
            ) {
            }
            async fn remove_utxo(&self, _output_id: &pms_types::OutputId) -> bool {
                false
            }
            async fn get_utxo(
                &self,
                _output_id: &pms_types::OutputId,
            ) -> Option<pms_types::TxOutput> {
                // Return a UTXO for the sender to test sender resolution
                Some(pms_types::TxOutput {
                    address: "sender_addr".into(),
                    amount: "100".into(),
                    asset_id: None,
                })
            }
        }

        let tx = Transaction {
            inputs: vec![TxInput {
                out: OutputId {
                    txid: "tx1".into(),
                    index: 0,
                },
            }],
            outputs: vec![out("receiver_addr", "90"), out("sender_addr", "9")],
            fee: "1".into(),
            unlocks: vec![],
        };

        let adapter = MockAdapter;
        let items = classify_activity(&PlainPayload::TxUtxo(tx), "receiver_addr", &adapter).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "transfer_in");
        assert_eq!(items[0].direction, "in");
        assert_eq!(items[0].amount.as_deref(), Some("90"));
        assert_eq!(items[0].counterparty.as_deref(), Some("sender_addr"));
    }

    #[tokio::test]
    async fn classify_tx_transfer_out_async() {
        use async_trait::async_trait;
        use pms_interface::NetDagAdapter;

        struct MockAdapter;

        #[async_trait]
        impl NetDagAdapter for MockAdapter {
            async fn have_block(&self, _id: &str) -> bool {
                false
            }
            async fn persist_block(
                &self,
                _wb: &pms_wire::WireBlock,
            ) -> anyhow::Result<pms_storage::PutResult> {
                Ok(pms_storage::PutResult::Inserted)
            }
            async fn broadcast_block(&self, _b: &pms_wire::WireBlock) -> anyhow::Result<()> {
                Ok(())
            }
            async fn top_tips(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
                Ok(vec![])
            }
            async fn get_block(&self, _id: &str) -> anyhow::Result<Option<pms_wire::WireBlock>> {
                Ok(None)
            }
            async fn recent_ids(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
                Ok(vec![])
            }
            async fn get_blocks_by_ids(
                &self,
                _ids: &[String],
            ) -> anyhow::Result<Vec<pms_wire::WireBlock>> {
                Ok(vec![])
            }
            fn min_pow_leading_zero_bits(&self) -> u8 {
                0
            }
            async fn circulating_supply(&self) -> (rust_decimal::Decimal, u64) {
                (rust_decimal::Decimal::ZERO, 0)
            }
            async fn circulating_supply_by_asset(
                &self,
                _asset_id: Option<&str>,
            ) -> (rust_decimal::Decimal, u64) {
                (rust_decimal::Decimal::ZERO, 0)
            }
            async fn balance_by_address(&self, _address: &str) -> rust_decimal::Decimal {
                rust_decimal::Decimal::ZERO
            }
            async fn utxos_by_address(
                &self,
                _address: &str,
            ) -> Vec<(pms_types::OutputId, pms_types::TxOutput)> {
                vec![]
            }
            async fn add_utxo(
                &self,
                _txid: String,
                _index: u32,
                _address: String,
                _amount: String,
                _asset_id: Option<String>,
            ) {
            }
            async fn remove_utxo(&self, _output_id: &pms_types::OutputId) -> bool {
                false
            }
            async fn get_utxo(
                &self,
                _output_id: &pms_types::OutputId,
            ) -> Option<pms_types::TxOutput> {
                Some(pms_types::TxOutput {
                    address: "sender_addr".into(),
                    amount: "100".into(),
                    asset_id: None,
                })
            }
        }

        let tx = Transaction {
            inputs: vec![TxInput {
                out: OutputId {
                    txid: "tx1".into(),
                    index: 0,
                },
            }],
            outputs: vec![out("bob", "99")],
            fee: "1".into(),
            unlocks: vec![],
        };

        let adapter = MockAdapter;
        let items = classify_activity(&PlainPayload::TxUtxo(tx), "sender_addr", &adapter).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "transfer_out");
        assert_eq!(items[0].direction, "out");
        assert_eq!(items[0].amount.as_deref(), Some("99"));
        assert_eq!(items[0].counterparty.as_deref(), Some("bob"));
    }

    #[tokio::test]
    async fn classify_tx_transfer_self_async() {
        use async_trait::async_trait;
        use pms_interface::NetDagAdapter;

        struct MockAdapter;

        #[async_trait]
        impl NetDagAdapter for MockAdapter {
            async fn have_block(&self, _id: &str) -> bool {
                false
            }
            async fn persist_block(
                &self,
                _wb: &pms_wire::WireBlock,
            ) -> anyhow::Result<pms_storage::PutResult> {
                Ok(pms_storage::PutResult::Inserted)
            }
            async fn broadcast_block(&self, _b: &pms_wire::WireBlock) -> anyhow::Result<()> {
                Ok(())
            }
            async fn top_tips(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
                Ok(vec![])
            }
            async fn get_block(&self, _id: &str) -> anyhow::Result<Option<pms_wire::WireBlock>> {
                Ok(None)
            }
            async fn recent_ids(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
                Ok(vec![])
            }
            async fn get_blocks_by_ids(
                &self,
                _ids: &[String],
            ) -> anyhow::Result<Vec<pms_wire::WireBlock>> {
                Ok(vec![])
            }
            fn min_pow_leading_zero_bits(&self) -> u8 {
                0
            }
            async fn circulating_supply(&self) -> (rust_decimal::Decimal, u64) {
                (rust_decimal::Decimal::ZERO, 0)
            }
            async fn circulating_supply_by_asset(
                &self,
                _asset_id: Option<&str>,
            ) -> (rust_decimal::Decimal, u64) {
                (rust_decimal::Decimal::ZERO, 0)
            }
            async fn balance_by_address(&self, _address: &str) -> rust_decimal::Decimal {
                rust_decimal::Decimal::ZERO
            }
            async fn utxos_by_address(
                &self,
                _address: &str,
            ) -> Vec<(pms_types::OutputId, pms_types::TxOutput)> {
                vec![]
            }
            async fn add_utxo(
                &self,
                _txid: String,
                _index: u32,
                _address: String,
                _amount: String,
                _asset_id: Option<String>,
            ) {
            }
            async fn remove_utxo(&self, _output_id: &pms_types::OutputId) -> bool {
                false
            }
            async fn get_utxo(
                &self,
                _output_id: &pms_types::OutputId,
            ) -> Option<pms_types::TxOutput> {
                Some(pms_types::TxOutput {
                    address: "alice".into(),
                    amount: "100".into(),
                    asset_id: None,
                })
            }
        }

        let tx = Transaction {
            inputs: vec![TxInput {
                out: OutputId {
                    txid: "tx1".into(),
                    index: 0,
                },
            }],
            outputs: vec![out("alice", "99")],
            fee: "1".into(),
            unlocks: vec![],
        };

        let adapter = MockAdapter;
        let items = classify_activity(&PlainPayload::TxUtxo(tx), "alice", &adapter).await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "transfer_self");
        assert_eq!(items[0].direction, "info");
        assert_eq!(items[0].amount.as_deref(), Some("99"));
    }

    // ── Mint with asset_id ───────────────────────────────────────────

    #[test]
    fn classify_mint_with_asset_id() {
        let plain = PlainPayload::Mint {
            outputs: vec![out_asset("alice", "1000", "edenite")],
        };
        let items = classify_activity_sync(&plain, "alice");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "mint");
        assert_eq!(items[0].asset_id.as_deref(), Some("edenite"));
    }
}
