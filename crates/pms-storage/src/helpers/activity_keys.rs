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

// ═══════════════════════════════════════════════════════════════════
// Per-address-per-type activity index helpers
// ═══════════════════════════════════════════════════════════════════

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
