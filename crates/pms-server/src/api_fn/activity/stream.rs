use crate::api::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use serde::Deserialize;
use std::collections::HashSet;
use std::convert::Infallible;

use super::handler::ActivityItem;
use super::classify::{classify_activity_sync, parse_type_filter};

/// Hard cap on the address set for `/v1/activity/stream?addresses=...`.
/// Prevents abuse (one client subscribing to millions of addresses).
/// SaaS watching > 1000 deposit addresses should shard across multiple
/// streams or fall back to webhook delivery.
pub const MAX_ADDRESSES_PER_STREAM: usize = 1000;

// ═══════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
pub struct StreamActivityQuery {
    #[serde(rename = "type")]
    pub filter_type: Option<String>,
    /// Optional X25519 private key (hex) for decrypting encrypted payloads in real-time.
    pub x25519_sk_hex: Option<String>,
}

#[derive(Deserialize)]
pub struct MultiAddressStreamQuery {
    /// Comma-separated address list, e.g. `?addresses=8e1a,8e1b,8e1c`.
    /// Capped at [`MAX_ADDRESSES_PER_STREAM`] — over-the-cap requests are
    /// rejected with 400 rather than silently truncated.
    pub addresses: Option<String>,
    #[serde(rename = "type")]
    pub filter_type: Option<String>,
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
                    if !(addr_match || is_encrypted && sk_opt.is_some()) {
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
                                // Trust involved_addresses from event: no involves_address check
                                Ok(decrypted) => decrypted,
                                _ => {
                                    // Decryption failed — emit encrypted fallback
                                    let item = ActivityItem {
                                        block_id: block_id.clone(),
                                        ts_ms,
                                        activity_type: "encrypted".to_string(),
                                        direction: "info".to_string(),
                                        amount: None,
                                        asset_id: None,
                                        counterparty: None,
                                        ledger_id: ledger_tag.clone(),
                                        payload: serde_json::json!({ "encrypted": true }),
                                    };
                                    if let Ok(json) = serde_json::to_string(&item) {
                                        yield Ok(Event::default().event("activity").data(json));
                                    }
                                    continue;
                                }
                            },
                            None => {
                                // No key — emit encrypted fallback if address is in involved_addresses
                                if addr_match {
                                    let item = ActivityItem {
                                        block_id: block_id.clone(),
                                        ts_ms,
                                        activity_type: "encrypted".to_string(),
                                        direction: "info".to_string(),
                                        amount: None,
                                        asset_id: None,
                                        counterparty: None,
                                        ledger_id: ledger_tag.clone(),
                                        payload: serde_json::json!({ "encrypted": true }),
                                    };
                                    if let Ok(json) = serde_json::to_string(&item) {
                                        yield Ok(Event::default().event("activity").data(json));
                                    }
                                }
                                continue;
                            }
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
// GET /v1/activity/stream?addresses=a,b,c (multi-address SSE)
// ═══════════════════════════════════════════════════════════════════
//
// Single SSE stream that watches an arbitrary set of addresses (up to
// `MAX_ADDRESSES_PER_STREAM`). Designed for SaaS payment-rail watchers:
// one server connection per N user deposit addresses instead of N
// connections — scales to 100K+ users at engine-side gateway capacity.
//
// **Encrypted payloads** are surfaced as a generic `encrypted` activity
// item with the matched address, so the SaaS knows "something happened
// involving address X" without needing the X25519 key. To get the
// decrypted detail it then calls `GET /v1/wallet/{X}/activity/stream`
// with the wallet key for that specific address.

pub async fn stream_multi_address_activity(
    State(app): State<AppState>,
    Query(q): Query<MultiAddressStreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
    // Parse + validate the address set up front so we surface 400 rather
    // than booting an empty stream that never emits.
    let raw = q.addresses.unwrap_or_default();
    let addrs: HashSet<String> = raw
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    if addrs.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "missing or empty `addresses=` query param".to_string(),
        ));
    }
    if addrs.len() > MAX_ADDRESSES_PER_STREAM {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "too many addresses ({}); limit is {} per stream — shard across multiple streams or use webhook delivery",
                addrs.len(),
                MAX_ADDRESSES_PER_STREAM
            ),
        ));
    }

    let bus = app.srv.adapter_arc().event_bus().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "event bus not available".to_string(),
    ))?;

    let mut rx = bus.subscribe();
    let type_filters: Vec<String> = parse_type_filter(&q.filter_type)
        .into_iter()
        .map(|s| s.to_string())
        .collect();
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
                    // Find the intersection between this block's involved
                    // addresses and the subscription set. The same block
                    // can match multiple subscribed addresses (e.g. a TX
                    // from one user's deposit to another user's payout
                    // wallet, both watched by the same SaaS).
                    let matched: Vec<String> = involved_addresses
                        .iter()
                        .filter(|a| addrs.contains(&a.to_lowercase()))
                        .cloned()
                        .collect();
                    if matched.is_empty() {
                        continue;
                    }

                    let env = match serde_json::from_str::<PayloadEnvelope>(&payload_json) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    // Encrypted: emit one `encrypted` notification per
                    // matched address (server can't decrypt). The SaaS
                    // upgrades to the per-wallet stream for detail.
                    if let PayloadEnvelope::Encrypted(_) | PayloadEnvelope::Plain(PlainPayload::EncryptedReward { .. }) = &env {
                        for addr in &matched {
                            let item = ActivityItem {
                                block_id: block_id.clone(),
                                ts_ms,
                                activity_type: "encrypted".to_string(),
                                direction: "info".to_string(),
                                amount: None,
                                asset_id: None,
                                counterparty: None,
                                ledger_id: ledger_tag.clone(),
                                payload: serde_json::json!({ "encrypted": true, "address": addr }),
                            };
                            if let Ok(json) = serde_json::to_string(&item) {
                                yield Ok(Event::default().event("activity").data(json));
                            }
                        }
                        continue;
                    }

                    let plain = match env {
                        PayloadEnvelope::Plain(p) => p,
                        _ => continue,
                    };

                    // Classify once per matched address — the activity
                    // semantics ("send/recv/mint") depend on which side
                    // of the TX the address sits on, so we can't pool.
                    for addr in &matched {
                        for mut item in classify_activity_sync(&plain, addr) {
                            if !type_filters.is_empty()
                                && !type_filters.iter().any(|f| f == &item.activity_type)
                            {
                                continue;
                            }
                            item.block_id = block_id.clone();
                            item.ts_ms = ts_ms;
                            item.ledger_id = ledger_tag.clone();
                            // Tag the matched address inside the payload
                            // so the SaaS can route to the right user
                            // record without re-parsing inputs/outputs.
                            if let Some(obj) = item.payload.as_object_mut() {
                                obj.insert(
                                    "address".to_string(),
                                    serde_json::Value::String(addr.clone()),
                                );
                            }
                            if let Ok(json) = serde_json::to_string(&item) {
                                yield Ok(Event::default().event("activity").data(json));
                            }
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        "Multi-address SSE stream lagged by {} events ({} addresses watched)",
                        n,
                        addrs.len()
                    );
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
