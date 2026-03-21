use std::time::{SystemTime, UNIX_EPOCH};

pub fn be_u64(x: u64) -> [u8; 8] {
    x.to_be_bytes()
}
/// Decode 8 big-endian bytes into i64. Returns 0 for malformed input.
pub fn from_be_i64(b: &[u8]) -> i64 {
    let Ok(arr): Result<[u8; 8], _> = b.try_into() else {
        return 0;
    };
    u64::from_be_bytes(arr) as i64
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
/// Decode 8+ big-endian bytes into a timestamp (i64). Returns 0 if input is too short.
pub fn be_to_ts(b: &[u8]) -> i64 {
    if b.len() < 8 {
        return 0;
    }
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
/// Decode 8+ little-endian bytes into u64. Returns 0 if input is too short.
pub fn le_to_u64(b: &[u8]) -> u64 {
    if b.len() < 8 {
        return 0;
    }
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&b[..8]);
    u64::from_le_bytes(arr)
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
