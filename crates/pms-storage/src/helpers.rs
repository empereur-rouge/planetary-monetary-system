use std::time::{SystemTime, UNIX_EPOCH};

pub fn be_u64(x: u64) -> [u8;8] { x.to_be_bytes() }
pub fn from_be_i64(b:&[u8]) -> i64 { u64::from_be_bytes(b.try_into().unwrap()) as i64 }
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

pub fn ts_to_be(ts: i64) -> [u8;8] {
    (ts as u64).to_be_bytes()
}
pub fn be_to_ts(b: &[u8]) -> i64 {
    let mut arr = [0u8;8];
    arr.copy_from_slice(&b[..8]);
    u64::from_be_bytes(arr) as i64
}
pub fn now_ms_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub fn u64_to_le(v: u64) -> [u8;8] {
    v.to_le_bytes()
}
pub fn le_to_u64(b: &[u8]) -> u64 {
    let mut arr = [0u8;8];
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
    if key.len() < 10 { return None; }
    let mut ts_be = [0u8;8];
    ts_be.copy_from_slice(&key[..8]);
    let ts_ms = u64::from_be_bytes(ts_be) as i64;
    let id = std::str::from_utf8(&key[9..]).ok()?.to_string();
    Some((ts_ms, id))
}

/// encode ts_ms (i64) en 8 bytes BE pour id2ts
pub fn ts_bytes(ts_ms: i64) -> [u8;8] {
    (ts_ms as u64).to_be_bytes()
}

pub fn be_to_i64(buf: &[u8]) -> anyhow::Result<i64> {
    if buf.len() != 8 {
        return Err(anyhow::anyhow!("invalid ts len"));
    }
    let mut arr = [0u8;8];
    arr.copy_from_slice(buf);
    let v = u64::from_be_bytes(arr);
    Ok(v as i64)
}