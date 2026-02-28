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
