use crate::activity_item::StoredActivityItem;
use pms_types_payload::PlainPayload;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn be_u64(x: u64) -> [u8; 8] {
    x.to_be_bytes()
}
pub fn from_be_i64(b: &[u8]) -> i64 {
    u64::from_be_bytes(b.try_into().unwrap()) as i64
}
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub fn ts_to_be(ts: i64) -> [u8; 8] {
    (ts as u64).to_be_bytes()
}
pub fn be_to_ts(b: &[u8]) -> i64 {
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&b[..8]);
    u64::from_be_bytes(arr) as i64
}
pub fn now_ms_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub fn u64_to_le(v: u64) -> [u8; 8] {
    v.to_le_bytes()
}
pub fn le_to_u64(b: &[u8]) -> u64 {
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&b[..8]);
    u64::from_le_bytes(arr)
}

/// encodage clé for time index:
/// key = [ts_be:8][0x00][block_id bytes]
pub fn key_time_index(ts_ms: i64, id: &str) -> Vec<u8> {
    let mut k = (ts_ms as u64).to_be_bytes().to_vec();
    k.push(0);
    k.extend_from_slice(id.as_bytes());
    k
}

/// helper inverse (optionnel) si tu veux debugger
pub fn parse_time_index_key(key: &[u8]) -> Option<(i64, String)> {
    if key.len() < 10 {
        return None;
    }
    let mut ts_be = [0u8; 8];
    ts_be.copy_from_slice(&key[..8]);
    let ts_ms = u64::from_be_bytes(ts_be) as i64;
    let id = std::str::from_utf8(&key[9..]).ok()?.to_string();
    Some((ts_ms, id))
}

/// encode ts_ms (i64) en 8 bytes BE pour id2ts
pub fn ts_bytes(ts_ms: i64) -> [u8; 8] {
    (ts_ms as u64).to_be_bytes()
}

pub fn be_to_i64(buf: &[u8]) -> anyhow::Result<i64> {
    if buf.len() != 8 {
        return Err(anyhow::anyhow!("invalid ts len"));
    }
    let mut arr = [0u8; 8];
    arr.copy_from_slice(buf);
    let v = u64::from_be_bytes(arr);
    Ok(v as i64)
}

// ═══════════════════════════════════════════════════════════════════
// Per-address activity index helpers
// ═══════════════════════════════════════════════════════════════════

/// Build key for the `addr_activity` CF.
/// Format: `[addr_bytes][0x00][ts_be:8][block_id_bytes]`
///
/// Addresses are bech32 (printable ASCII, no null bytes), so 0x00 is an
/// unambiguous separator.  Big-endian timestamp within the address prefix
/// lets us reverse-iterate newest-first.
pub fn key_addr_activity(addr: &str, ts_ms: i64, block_id: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(addr.len() + 1 + 8 + block_id.len());
    k.extend_from_slice(addr.as_bytes());
    k.push(0);
    k.extend_from_slice(&(ts_ms as u64).to_be_bytes());
    k.extend_from_slice(block_id.as_bytes());
    k
}

/// Build prefix for iterating all activity entries of a specific address.
/// Format: `[addr_bytes][0x00]`
pub fn prefix_addr_activity(addr: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(addr.len() + 1);
    k.extend_from_slice(addr.as_bytes());
    k.push(0);
    k
}

/// Parse an `addr_activity` key, given the known address length.
/// Returns `(ts_ms, block_id)`.
pub fn parse_addr_activity_key(key: &[u8], addr_len: usize) -> Option<(i64, String)> {
    // key = [addr:addr_len][0x00][ts:8][block_id:...]
    let min_len = addr_len + 1 + 8;
    if key.len() < min_len {
        return None;
    }
    if key[addr_len] != 0 {
        return None;
    }
    let ts_start = addr_len + 1;
    let mut ts_be = [0u8; 8];
    ts_be.copy_from_slice(&key[ts_start..ts_start + 8]);
    let ts_ms = u64::from_be_bytes(ts_be) as i64;
    let id = std::str::from_utf8(&key[ts_start + 8..]).ok()?.to_string();
    Some((ts_ms, id))
}

/// Extract all addresses involved in a `PlainPayload`.
///
/// Replicates `pms_wallet::history::collect_involved_addresses` to avoid a
/// circular dependency (pms-wallet depends on pms-storage).
pub fn extract_involved_addresses(plain: &PlainPayload) -> Vec<String> {
    let mut addrs = Vec::new();
    match plain {
        PlainPayload::Mint { outputs } => {
            addrs.extend(outputs.iter().map(|o| o.address.clone()));
        }
        PlainPayload::TxUtxo(tx) => {
            addrs.extend(tx.outputs.iter().map(|o| o.address.clone()));
        }
        PlainPayload::Reward {
            fee_outputs,
            reward_outputs,
            ..
        } => {
            addrs.extend(fee_outputs.iter().map(|o| o.address.clone()));
            addrs.extend(reward_outputs.iter().map(|o| o.address.clone()));
        }
        PlainPayload::Nft(action) => match action {
            pms_types_nft::NftAction::Mint { creator, .. } => addrs.push(creator.clone()),
            pms_types_nft::NftAction::Transfer { from, to, .. } => {
                addrs.push(from.clone());
                addrs.push(to.clone());
            }
            pms_types_nft::NftAction::Use { user, .. } => addrs.push(user.clone()),
            pms_types_nft::NftAction::Burn { burner, .. } => addrs.push(burner.clone()),
            pms_types_nft::NftAction::BatchBurn { burner, .. } => addrs.push(burner.clone()),
        },
        PlainPayload::TokenCreate(meta) => addrs.push(meta.creator.clone()),
        PlainPayload::BridgeLock { dest_address, .. } => addrs.push(dest_address.clone()),
        PlainPayload::BridgeMint { outputs, .. } => {
            addrs.extend(outputs.iter().map(|o| o.address.clone()));
        }
        PlainPayload::Freeze { address, .. } | PlainPayload::Unfreeze { address, .. } => {
            addrs.push(address.clone());
        }
        PlainPayload::Seize {
            from_address,
            outputs,
            ..
        } => {
            addrs.push(from_address.clone());
            addrs.extend(outputs.iter().map(|o| o.address.clone()));
        }
        PlainPayload::Reverse { outputs, .. } => {
            addrs.extend(outputs.iter().map(|o| o.address.clone()));
        }
        _other => {
            tracing::trace!("extract_addresses_from_payload: skipped variant (no extractable addresses)");
        }
    }
    addrs.dedup();
    addrs
}

// ═══════════════════════════════════════════════════════════════════
// Per-address-per-type activity index helpers
// ═══════════════════════════════════════════════════════════════════

/// Activity categories for the `addr_type_activity` CF.
/// Each variant maps to a 1-byte discriminant used as part of the key.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityCategory {
    Mint = 1,
    Transfer = 2,
    Fee = 3,
    Reward = 4,
    Nft = 5,
    TokenCreate = 6,
    Bridge = 7,
    Compliance = 8,
    Reverse = 9,
}

impl ActivityCategory {
    /// Map an API `filter_type` string to a category.
    pub fn from_filter_type(s: &str) -> Option<Self> {
        match s {
            "mint" => Some(Self::Mint),
            "transfer_in" | "transfer_out" | "transfer_self" => Some(Self::Transfer),
            "fee_received" => Some(Self::Fee),
            "reward" => Some(Self::Reward),
            "nft_mint" | "nft_transfer_in" | "nft_transfer_out" | "nft_burn" | "nft_use" => {
                Some(Self::Nft)
            }
            "token_create" => Some(Self::TokenCreate),
            "bridge_lock_in" | "bridge_mint" => Some(Self::Bridge),
            "freeze" | "unfreeze" | "seized" | "seize_received" => Some(Self::Compliance),
            "reverse_received" => Some(Self::Reverse),
            _ => None,
        }
    }

    pub fn as_byte(self) -> u8 {
        self as u8
    }
}

/// Extract `(address, category)` pairs from a `PlainPayload`.
///
/// Unlike `extract_involved_addresses`, this distinguishes between fee and
/// reward addresses in `Reward` blocks, assigning each its own category.
pub fn extract_involved_with_category(plain: &PlainPayload) -> Vec<(String, ActivityCategory)> {
    let mut out = Vec::new();
    match plain {
        PlainPayload::Mint { outputs } => {
            for o in outputs {
                out.push((o.address.clone(), ActivityCategory::Mint));
            }
        }
        PlainPayload::TxUtxo(tx) => {
            for o in &tx.outputs {
                // Fee outputs (amount == tx.fee) go under Fee category so that
                // ?type=transfer_in never returns fee-collector entries.
                let cat = if is_fee_output_only(tx, &o.address) {
                    ActivityCategory::Fee
                } else {
                    ActivityCategory::Transfer
                };
                out.push((o.address.clone(), cat));
            }
        }
        PlainPayload::Reward {
            fee_outputs,
            reward_outputs,
            ..
        } => {
            for o in fee_outputs {
                out.push((o.address.clone(), ActivityCategory::Fee));
            }
            for o in reward_outputs {
                out.push((o.address.clone(), ActivityCategory::Reward));
            }
        }
        PlainPayload::Nft(action) => match action {
            pms_types_nft::NftAction::Mint { creator, .. } => {
                out.push((creator.clone(), ActivityCategory::Nft));
            }
            pms_types_nft::NftAction::Transfer { from, to, .. } => {
                out.push((from.clone(), ActivityCategory::Nft));
                out.push((to.clone(), ActivityCategory::Nft));
            }
            pms_types_nft::NftAction::Use { user, .. } => {
                out.push((user.clone(), ActivityCategory::Nft));
            }
            pms_types_nft::NftAction::Burn { burner, .. } => {
                out.push((burner.clone(), ActivityCategory::Nft));
            }
            pms_types_nft::NftAction::BatchBurn { burner, .. } => {
                out.push((burner.clone(), ActivityCategory::Nft));
            }
        },
        PlainPayload::TokenCreate(meta) => {
            out.push((meta.creator.clone(), ActivityCategory::TokenCreate));
        }
        PlainPayload::BridgeLock { dest_address, .. } => {
            out.push((dest_address.clone(), ActivityCategory::Bridge));
        }
        PlainPayload::BridgeMint { outputs, .. } => {
            for o in outputs {
                out.push((o.address.clone(), ActivityCategory::Bridge));
            }
        }
        PlainPayload::Freeze { address, .. } | PlainPayload::Unfreeze { address, .. } => {
            out.push((address.clone(), ActivityCategory::Compliance));
        }
        PlainPayload::Seize {
            from_address,
            outputs,
            ..
        } => {
            out.push((from_address.clone(), ActivityCategory::Compliance));
            for o in outputs {
                out.push((o.address.clone(), ActivityCategory::Compliance));
            }
        }
        PlainPayload::Reverse { outputs, .. } => {
            for o in outputs {
                out.push((o.address.clone(), ActivityCategory::Reverse));
            }
        }
        _other => {
            tracing::trace!("addr_activity_pairs: skipped variant (no per-address activity)");
        }
    }
    out
}

/// Build key for the `addr_type_activity` CF.
/// Format: `[addr_bytes][0x00][category:1][ts_be:8][block_id_bytes]`
pub fn key_addr_type_activity(addr: &str, cat: u8, ts_ms: i64, block_id: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(addr.len() + 1 + 1 + 8 + block_id.len());
    k.extend_from_slice(addr.as_bytes());
    k.push(0);
    k.push(cat);
    k.extend_from_slice(&(ts_ms as u64).to_be_bytes());
    k.extend_from_slice(block_id.as_bytes());
    k
}

/// Build prefix for iterating a specific category for an address.
/// Format: `[addr_bytes][0x00][category:1]`
pub fn prefix_addr_type_activity(addr: &str, cat: u8) -> Vec<u8> {
    let mut k = Vec::with_capacity(addr.len() + 2);
    k.extend_from_slice(addr.as_bytes());
    k.push(0);
    k.push(cat);
    k
}

/// Parse an `addr_type_activity` key, given the known address length.
/// Returns `(category, ts_ms, block_id)`.
pub fn parse_addr_type_activity_key(key: &[u8], addr_len: usize) -> Option<(u8, i64, String)> {
    // key = [addr:addr_len][0x00][cat:1][ts:8][block_id:...]
    let min_len = addr_len + 1 + 1 + 8;
    if key.len() < min_len {
        return None;
    }
    if key[addr_len] != 0 {
        return None;
    }
    let cat = key[addr_len + 1];
    let ts_start = addr_len + 2;
    let mut ts_be = [0u8; 8];
    ts_be.copy_from_slice(&key[ts_start..ts_start + 8]);
    let ts_ms = u64::from_be_bytes(ts_be) as i64;
    let id = std::str::from_utf8(&key[ts_start + 8..]).ok()?.to_string();
    Some((cat, ts_ms, id))
}

// ═══════════════════════════════════════════════════════════════════════════════
// Pre-computed activity items (written at block persist time, read at query time)
// ═══════════════════════════════════════════════════════════════════════════════

/// Check if an address is solely a fee recipient in a TxUtxo.
///
/// Returns `true` when **all** outputs addressed to `addr` account for exactly the
/// transaction fee — i.e. the address only appears in the transaction as the fee
/// collector, not as a regular transfer recipient.
pub fn is_fee_output_only(tx: &pms_types::Transaction, addr: &str) -> bool {
    let fee = match tx.fee.parse::<rust_decimal::Decimal>() {
        Ok(f) if f > rust_decimal::Decimal::ZERO => f,
        _ => return false,
    };

    let addr_total: rust_decimal::Decimal = tx
        .outputs
        .iter()
        .filter(|o| o.address == addr)
        .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
        .sum();

    addr_total == fee
}

/// Classify a `PlainPayload` into `StoredActivityItem`s for a given address.
///
/// This is the storage-layer equivalent of `classify_activity()` in pms-server.
/// It is **synchronous**: the caller must pre-resolve the sender address for TxUtxo
/// and pass it as `sender_addr`.
pub fn classify_for_storage(
    plain: &PlainPayload,
    addr: &str,
    sender_addr: Option<&str>,
) -> Vec<StoredActivityItem> {
    match plain {
        PlainPayload::Mint { outputs } => outputs
            .iter()
            .filter(|o| o.address == addr)
            .map(|o| StoredActivityItem {
                activity_type: "mint".into(),
                direction: "in".into(),
                amount: Some(o.amount.clone()),
                asset_id: o.asset_id.clone(),
                counterparty: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            })
            .collect(),

        PlainPayload::TxUtxo(tx) => {
            let is_sender = sender_addr == Some(addr);
            let is_receiver = tx.outputs.iter().any(|o| o.address == addr);
            let has_other_recipients = tx.outputs.iter().any(|o| o.address != addr);
            let payload_val = serde_json::to_value(tx).unwrap_or_default();
            let mut items = Vec::new();

            if is_sender && is_receiver && !has_other_recipients {
                // True self-transfer (consolidation): ALL outputs go back to sender
                let net: rust_decimal::Decimal = tx
                    .outputs
                    .iter()
                    .filter(|o| o.address == addr)
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                items.push(StoredActivityItem {
                    activity_type: "transfer_self".into(),
                    direction: "info".into(),
                    amount: Some(net.to_string()),
                    asset_id: tx.outputs.first().and_then(|o| o.asset_id.clone()),
                    counterparty: None,
                    payload: payload_val,
                });
            } else if is_sender {
                // Transfer out (with or without change back to sender)
                let recipient = tx
                    .outputs
                    .iter()
                    .find(|o| o.address != addr)
                    .map(|o| o.address.clone());
                let sent: rust_decimal::Decimal = tx
                    .outputs
                    .iter()
                    .filter(|o| o.address != addr)
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                items.push(StoredActivityItem {
                    activity_type: "transfer_out".into(),
                    direction: "out".into(),
                    amount: Some(sent.to_string()),
                    asset_id: tx.outputs.first().and_then(|o| o.asset_id.clone()),
                    counterparty: recipient,
                    payload: payload_val,
                });
            } else if is_receiver {
                let received: rust_decimal::Decimal = tx
                    .outputs
                    .iter()
                    .filter(|o| o.address == addr)
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                let activity_type = if is_fee_output_only(tx, addr) {
                    "fee_received"
                } else {
                    "transfer_in"
                };
                items.push(StoredActivityItem {
                    activity_type: activity_type.into(),
                    direction: "in".into(),
                    amount: Some(received.to_string()),
                    asset_id: tx
                        .outputs
                        .iter()
                        .find(|o| o.address == addr)
                        .and_then(|o| o.asset_id.clone()),
                    counterparty: sender_addr.map(String::from),
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
                items.push(StoredActivityItem {
                    activity_type: "fee_received".into(),
                    direction: "in".into(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: None,
                    payload: payload_val.clone(),
                });
            }
            for o in reward_outputs.iter().filter(|o| o.address == addr) {
                items.push(StoredActivityItem {
                    activity_type: "reward".into(),
                    direction: "in".into(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: None,
                    payload: payload_val.clone(),
                });
            }
            items
        }

        PlainPayload::Nft(action) => {
            let payload_val = serde_json::to_value(action).unwrap_or_default();
            match action {
                pms_types_nft::NftAction::Mint { creator, .. } if creator == addr => {
                    vec![StoredActivityItem {
                        activity_type: "nft_mint".into(),
                        direction: "in".into(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        payload: payload_val,
                    }]
                }
                pms_types_nft::NftAction::Transfer { from, to, .. } => {
                    if to == addr {
                        vec![StoredActivityItem {
                            activity_type: "nft_transfer_in".into(),
                            direction: "in".into(),
                            amount: None,
                            asset_id: None,
                            counterparty: Some(from.clone()),
                            payload: payload_val,
                        }]
                    } else if from == addr {
                        vec![StoredActivityItem {
                            activity_type: "nft_transfer_out".into(),
                            direction: "out".into(),
                            amount: None,
                            asset_id: None,
                            counterparty: Some(to.clone()),
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
                    vec![StoredActivityItem {
                        activity_type: "nft_burn".into(),
                        direction: "out".into(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        payload: payload_val,
                    }]
                }
                pms_types_nft::NftAction::Use { user, .. } if user == addr => {
                    vec![StoredActivityItem {
                        activity_type: "nft_use".into(),
                        direction: "info".into(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        payload: payload_val,
                    }]
                }
                _ => vec![],
            }
        }

        PlainPayload::TokenCreate(meta) if meta.creator == addr => {
            vec![StoredActivityItem {
                activity_type: "token_create".into(),
                direction: "info".into(),
                amount: None,
                asset_id: Some(meta.asset_id.clone()),
                counterparty: None,
                payload: serde_json::to_value(meta).unwrap_or_default(),
            }]
        }

        PlainPayload::BridgeLock {
            dest_address,
            amount,
            asset_id,
            ..
        } if dest_address == addr => {
            vec![StoredActivityItem {
                activity_type: "bridge_lock_in".into(),
                direction: "in".into(),
                amount: Some(amount.clone()),
                asset_id: asset_id.clone(),
                counterparty: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            }]
        }

        PlainPayload::BridgeMint { outputs, .. } => outputs
            .iter()
            .filter(|o| o.address == addr)
            .map(|o| StoredActivityItem {
                activity_type: "bridge_mint".into(),
                direction: "in".into(),
                amount: Some(o.amount.clone()),
                asset_id: o.asset_id.clone(),
                counterparty: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            })
            .collect(),

        PlainPayload::Freeze {
            address, reason, ..
        } if address == addr => {
            vec![StoredActivityItem {
                activity_type: "freeze".into(),
                direction: "info".into(),
                amount: None,
                asset_id: None,
                counterparty: None,
                payload: serde_json::json!({ "reason": reason }),
            }]
        }

        PlainPayload::Unfreeze {
            address, reason, ..
        } if address == addr => {
            vec![StoredActivityItem {
                activity_type: "unfreeze".into(),
                direction: "info".into(),
                amount: None,
                asset_id: None,
                counterparty: None,
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
                items.push(StoredActivityItem {
                    activity_type: "seized".into(),
                    direction: "out".into(),
                    amount: Some(total.to_string()),
                    asset_id: None,
                    counterparty: None,
                    payload: payload_val.clone(),
                });
            }
            for o in outputs.iter().filter(|o| o.address == addr) {
                items.push(StoredActivityItem {
                    activity_type: "seize_received".into(),
                    direction: "in".into(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: Some(from_address.clone()),
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
                .map(|o| StoredActivityItem {
                    activity_type: "reverse_received".into(),
                    direction: "in".into(),
                    amount: Some(o.amount.clone()),
                    asset_id: o.asset_id.clone(),
                    counterparty: None,
                    payload: payload_val.clone(),
                })
                .collect()
        }

        _ => vec![],
    }
}

/// Pre-compute activity items for ALL involved addresses in one pass.
///
/// Returns a map: `address → Vec<StoredActivityItem>`.
/// The caller must pre-resolve the TxUtxo sender address and pass it in.
pub fn precompute_all_items(
    plain: &PlainPayload,
    involved_addrs: &[String],
    sender_addr: Option<&str>,
) -> HashMap<String, Vec<StoredActivityItem>> {
    let mut map = HashMap::new();
    for addr in involved_addrs {
        let items = classify_for_storage(plain, addr, sender_addr);
        if !items.is_empty() {
            map.insert(addr.clone(), items);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use pms_types::{Transaction, TxOutput};

    fn out(addr: &str, amount: &str) -> TxOutput {
        TxOutput {
            address: addr.into(),
            amount: amount.into(),
            asset_id: None,
        }
    }

    // ── is_fee_output_only ──────────────────────────────────────────

    #[test]
    fn is_fee_output_only_basic() {
        let tx = Transaction {
            inputs: vec![],
            outputs: vec![out("receiver", "99"), out("admin", "1")],
            fee: "1".into(),
            unlocks: vec![],
        };
        println!("admin (amount=1, fee=1): {}", is_fee_output_only(&tx, "admin"));
        println!("receiver (amount=99, fee=1): {}", is_fee_output_only(&tx, "receiver"));
        assert!(is_fee_output_only(&tx, "admin"));
        assert!(!is_fee_output_only(&tx, "receiver"));
    }

    #[test]
    fn is_fee_output_only_zero_fee() {
        let tx = Transaction {
            inputs: vec![],
            outputs: vec![out("alice", "100")],
            fee: "0".into(),
            unlocks: vec![],
        };
        println!("zero fee → {}", is_fee_output_only(&tx, "alice"));
        assert!(!is_fee_output_only(&tx, "alice"));
    }

    #[test]
    fn is_fee_output_only_decimal() {
        let tx = Transaction {
            inputs: vec![],
            outputs: vec![out("bob", "9.9996999"), out("treasury", "0.0003001")],
            fee: "0.0003001".into(),
            unlocks: vec![],
        };
        println!("treasury (0.0003001 == fee): {}", is_fee_output_only(&tx, "treasury"));
        println!("bob (9.9996999 != fee): {}", is_fee_output_only(&tx, "bob"));
        assert!(is_fee_output_only(&tx, "treasury"));
        assert!(!is_fee_output_only(&tx, "bob"));
    }

    #[test]
    fn is_fee_output_only_addr_not_in_outputs() {
        let tx = Transaction {
            inputs: vec![],
            outputs: vec![out("bob", "99"), out("admin", "1")],
            fee: "1".into(),
            unlocks: vec![],
        };
        // Address not in outputs → sum=0 ≠ fee → false
        println!("unknown addr: {}", is_fee_output_only(&tx, "unknown"));
        assert!(!is_fee_output_only(&tx, "unknown"));
    }

    // ── classify_for_storage: fee detection ─────────────────────────

    #[test]
    fn classify_for_storage_fee_receiver() {
        let tx = Transaction {
            inputs: vec![],
            outputs: vec![out("receiver", "99"), out("admin", "1")],
            fee: "1".into(),
            unlocks: vec![],
        };
        let plain = PlainPayload::TxUtxo(tx);

        // admin is fee-only receiver → fee_received
        let items = classify_for_storage(&plain, "admin", None);
        println!("admin items: {:?}", items.iter().map(|i| (&i.activity_type, &i.amount)).collect::<Vec<_>>());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "fee_received");
        assert_eq!(items[0].amount.as_deref(), Some("1"));

        // receiver is NOT fee → transfer_in
        let items = classify_for_storage(&plain, "receiver", None);
        println!("receiver items: {:?}", items.iter().map(|i| (&i.activity_type, &i.amount)).collect::<Vec<_>>());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "transfer_in");
    }

    #[test]
    fn classify_for_storage_sender_with_fee_output() {
        let tx = Transaction {
            inputs: vec![],
            outputs: vec![out("bob", "99"), out("admin", "1")],
            fee: "1".into(),
            unlocks: vec![],
        };
        let plain = PlainPayload::TxUtxo(tx);

        // sender sees transfer_out (fee detection only applies to receivers)
        let items = classify_for_storage(&plain, "sender", Some("sender"));
        println!("sender items: {:?}", items.iter().map(|i| (&i.activity_type, &i.amount)).collect::<Vec<_>>());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].activity_type, "transfer_out");
        // total sent to non-sender outputs: bob(99) + admin(1) = 100
        assert_eq!(items[0].amount.as_deref(), Some("100"));
    }

    // ── extract_involved_with_category: fee detection ───────────────

    #[test]
    fn extract_category_txutxo_with_fee_output() {
        let tx = Transaction {
            inputs: vec![],
            outputs: vec![out("receiver", "99"), out("admin", "1")],
            fee: "1".into(),
            unlocks: vec![],
        };
        let plain = PlainPayload::TxUtxo(tx);
        let cats = extract_involved_with_category(&plain);
        println!("categories: {:?}", cats.iter().map(|(a, c)| (a.as_str(), *c)).collect::<Vec<_>>());

        // receiver → Transfer, admin → Fee
        assert_eq!(cats.len(), 2);
        let admin_cat = cats.iter().find(|(a, _)| a == "admin").unwrap();
        let recv_cat = cats.iter().find(|(a, _)| a == "receiver").unwrap();
        assert_eq!(admin_cat.1, ActivityCategory::Fee);
        assert_eq!(recv_cat.1, ActivityCategory::Transfer);
    }

    #[test]
    fn extract_category_txutxo_no_fee_output() {
        // fee is 1 but nobody receives exactly 1 → all Transfer
        let tx = Transaction {
            inputs: vec![],
            outputs: vec![out("alice", "50"), out("bob", "49")],
            fee: "1".into(),
            unlocks: vec![],
        };
        let plain = PlainPayload::TxUtxo(tx);
        let cats = extract_involved_with_category(&plain);
        println!("categories (no fee output): {:?}", cats.iter().map(|(a, c)| (a.as_str(), *c)).collect::<Vec<_>>());

        assert!(cats.iter().all(|(_, c)| *c == ActivityCategory::Transfer));
    }

    #[test]
    fn extract_category_txutxo_decimal_fee() {
        let tx = Transaction {
            inputs: vec![],
            outputs: vec![out("bob", "9.9996999"), out("treasury", "0.0003001")],
            fee: "0.0003001".into(),
            unlocks: vec![],
        };
        let plain = PlainPayload::TxUtxo(tx);
        let cats = extract_involved_with_category(&plain);
        println!("decimal fee categories: {:?}", cats.iter().map(|(a, c)| (a.as_str(), *c)).collect::<Vec<_>>());

        let treasury_cat = cats.iter().find(|(a, _)| a == "treasury").unwrap();
        let bob_cat = cats.iter().find(|(a, _)| a == "bob").unwrap();
        assert_eq!(treasury_cat.1, ActivityCategory::Fee);
        assert_eq!(bob_cat.1, ActivityCategory::Transfer);
    }

    // ── precompute_all_items: fee detection ─────────────────────────

    #[test]
    fn precompute_all_items_fee_output() {
        let tx = Transaction {
            inputs: vec![],
            outputs: vec![out("receiver", "99"), out("admin", "1")],
            fee: "1".into(),
            unlocks: vec![],
        };
        let plain = PlainPayload::TxUtxo(tx);
        let addrs = extract_involved_addresses(&plain);
        let items_map = precompute_all_items(&plain, &addrs, None);

        println!("precompute keys: {:?}", items_map.keys().collect::<Vec<_>>());
        for (addr, items) in &items_map {
            println!("  {}: {:?}", addr, items.iter().map(|i| &i.activity_type).collect::<Vec<_>>());
        }

        let admin_items = items_map.get("admin").unwrap();
        assert_eq!(admin_items[0].activity_type, "fee_received");

        let recv_items = items_map.get("receiver").unwrap();
        assert_eq!(recv_items[0].activity_type, "transfer_in");
    }
}
