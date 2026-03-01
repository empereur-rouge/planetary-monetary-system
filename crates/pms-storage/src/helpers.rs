use pms_types_payload::PlainPayload;
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
        // EncryptedReward, Genesis, Milestone, ConfigUpdate: no extractable addresses
        _ => {}
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
                out.push((o.address.clone(), ActivityCategory::Transfer));
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
        _ => {}
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
