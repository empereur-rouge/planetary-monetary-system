/// encodage cle for time index:
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
