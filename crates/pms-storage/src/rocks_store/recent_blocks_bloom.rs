//! In-RAM Bloom filter of recently persisted block IDs.
//!
//! `append_blocks_batch` does a `multi_get_cf` on `cf_blocks` to detect
//! duplicates before building the WriteBatch. As the DAG grew past
//! ~700K blocks the parent block IDs aged out of the memtable and the
//! lookup walked into L0/L1 SSTs — the dedup sub-stage became the
//! dominant cost (≈47% of consumer time at 11h, sub-linear but
//! unbounded). Most blocks reaching the consumer are fresh; the
//! `multi_get_cf` round-trip pays for the rare duplicate at the cost
//! of every fast-path block.
//!
//! This module fronts that lookup with a rotating Bloom filter:
//!   - A negative answer ("not in filter") is authoritative — the
//!     block has never been inserted, so we skip the DB read.
//!   - A positive answer ("maybe in filter") falls back to
//!     `multi_get_cf` to confirm.
//!
//! Correctness depends on **every persisted block_id being inserted
//! into the filter**:
//!   1. After `append_blocks_batch` succeeds, the new IDs are
//!      added to the front segment.
//!   2. Single-block atomic-append paths (`append_block_atomic`,
//!      `append_block_atomic_with_utxo`) also insert.
//!   3. On startup, the filter is warmed from the `by_time` CF
//!      (newest-first walk, bounded), so post-restart blocks aren't
//!      mis-classified as new.
//!
//! Memory: rotating 2-segment design caps RAM. Each segment is sized
//! for `capacity` entries at ≈10 bits/entry, k=7 hash functions →
//! ≈0.8% false-positive rate. With `capacity = 5_000_000`, total
//! footprint ≈ 12 MB (6 MB × 2 segments).
//!
//! Hashing: block IDs are 64-char hex of a 256-bit random output, so
//! we extract two 64-bit halves directly from the first 32 hex chars
//! and use Kirsch-Mitzenmacher double hashing — no additional hash
//! function needed.

/// Decode a single ASCII hex character. Returns 0 for malformed input
/// (block IDs are always well-formed hex; this is just defense).
#[inline(always)]
fn hex_byte(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

/// Extract two 64-bit hash seeds from the first 32 hex chars of a
/// block ID. SHA-256 outputs are uniformly distributed so no further
/// mixing is required.
#[inline]
fn hash_pair(id: &[u8]) -> (u64, u64) {
    if id.len() >= 32 {
        let mut h1 = [0u8; 8];
        let mut h2 = [0u8; 8];
        for i in 0..8 {
            h1[i] = (hex_byte(id[2 * i]) << 4) | hex_byte(id[2 * i + 1]);
            h2[i] = (hex_byte(id[16 + 2 * i]) << 4) | hex_byte(id[16 + 2 * i + 1]);
        }
        (u64::from_le_bytes(h1), u64::from_le_bytes(h2))
    } else {
        // Fallback FNV-1a 64-bit for short / non-hex IDs (test fixtures).
        let mut a: u64 = 0xcbf29ce484222325;
        let mut b: u64 = 0x9e3779b97f4a7c15;
        for &byte in id {
            a ^= byte as u64;
            a = a.wrapping_mul(0x100000001b3);
            b = b.rotate_left(7) ^ (byte as u64);
            b = b.wrapping_mul(0x9e3779b97f4a7c15);
        }
        (a.max(1), b.max(1))
    }
}

const BITS_PER_ENTRY: usize = 10;
const HASH_COUNT: usize = 7;

struct BloomSegment {
    bits: Vec<u64>,
    bits_count: usize,
    inserted: usize,
}

impl BloomSegment {
    fn new(capacity: usize) -> Self {
        let total_bits = capacity.saturating_mul(BITS_PER_ENTRY).max(64);
        let words = total_bits.div_ceil(64);
        Self {
            bits: vec![0u64; words],
            bits_count: words * 64,
            inserted: 0,
        }
    }

    #[inline]
    fn position(&self, base: u64, step: u64, i: u64) -> usize {
        (base.wrapping_add(i.wrapping_mul(step)) as usize) % self.bits_count
    }

    fn contains(&self, id: &[u8]) -> bool {
        let (h1, h2) = hash_pair(id);
        for i in 0..HASH_COUNT {
            let pos = self.position(h1, h2, i as u64);
            let word = pos >> 6;
            let bit = pos & 63;
            // SAFETY-equivalent: word is bounded by `bits_count / 64` = `bits.len()`.
            if self.bits[word] & (1u64 << bit) == 0 {
                return false;
            }
        }
        true
    }

    fn insert(&mut self, id: &[u8]) {
        let (h1, h2) = hash_pair(id);
        for i in 0..HASH_COUNT {
            let pos = self.position(h1, h2, i as u64);
            let word = pos >> 6;
            let bit = pos & 63;
            self.bits[word] |= 1u64 << bit;
        }
        self.inserted += 1;
    }
}

/// Rotating two-segment Bloom filter.
///
/// Inserts go into `front`. When `front` reaches `capacity` entries we
/// rotate: drop `back`, move `front` → `back`, allocate a fresh `front`.
/// Lookups consult both segments. This bounds memory at ≈ 2 × `capacity`
/// × `BITS_PER_ENTRY` bits while keeping recent IDs queryable for at
/// least one full capacity window.
pub struct RecentBlocksBloom {
    front: BloomSegment,
    back: BloomSegment,
    capacity: usize,
    /// Set to `true` once the filter has been warmed from the DB. Until
    /// then, callers should treat `maybe_contains` as always-true (i.e.
    /// fall back to the DB lookup) so post-restart blocks aren't
    /// misclassified as never-seen.
    warmed: bool,
}

impl RecentBlocksBloom {
    /// `capacity` is the entry count per segment before rotation.
    /// Sensible default: 5M (covers ≈4h of clicker-grade traffic at
    /// 350 blk/s before rotation, ≈12MB total RAM).
    pub fn new(capacity: usize) -> Self {
        Self {
            front: BloomSegment::new(capacity),
            back: BloomSegment::new(capacity),
            capacity,
            warmed: false,
        }
    }

    /// Returns `true` if the ID **might** be in the filter. A `false`
    /// answer is authoritative iff `is_warmed()` is true.
    #[inline]
    pub fn maybe_contains(&self, id: &[u8]) -> bool {
        self.front.contains(id) || self.back.contains(id)
    }

    /// Insert and rotate if the front segment is full.
    pub fn insert(&mut self, id: &[u8]) {
        if self.front.inserted >= self.capacity {
            self.back = std::mem::replace(
                &mut self.front,
                BloomSegment::new(self.capacity),
            );
        }
        self.front.insert(id);
    }

    /// Mark the filter as warmed — typically called by the bootstrap
    /// loader after walking the recent block window.
    pub fn mark_warmed(&mut self) {
        self.warmed = true;
    }

    pub fn is_warmed(&self) -> bool {
        self.warmed
    }

    /// `(front_count, back_count, capacity)` — exposed for the metrics
    /// sampler to track filter saturation.
    pub fn stats(&self) -> (usize, usize, usize) {
        (self.front.inserted, self.back.inserted, self.capacity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SplitMix64 — a high-quality, decorrelated 64→64 PRNG used to
    /// produce SHA-256-grade synthetic block IDs for the tests below.
    /// Without good mixing, FPR on a Bloom filter inflates because the
    /// filter's hash positions become correlated.
    fn splitmix64(mut x: u64) -> u64 {
        x = x.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    fn make_id(seed: u64) -> String {
        let mut bytes = [0u8; 32];
        let mut state = seed;
        for i in 0..4 {
            state = splitmix64(state);
            bytes[i * 8..(i + 1) * 8].copy_from_slice(&state.to_le_bytes());
        }
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    #[test]
    fn no_false_negatives() {
        let mut bloom = RecentBlocksBloom::new(10_000);
        let ids: Vec<String> = (0..5_000).map(make_id).collect();
        for id in &ids {
            bloom.insert(id.as_bytes());
        }
        for id in &ids {
            assert!(bloom.maybe_contains(id.as_bytes()), "missing: {id}");
        }
    }

    #[test]
    fn false_positive_rate_under_2pct() {
        let mut bloom = RecentBlocksBloom::new(10_000);
        let inserted: Vec<String> = (0..10_000).map(make_id).collect();
        for id in &inserted {
            bloom.insert(id.as_bytes());
        }
        let mut fp = 0usize;
        let probes: u64 = 10_000;
        for i in 1_000_000..1_000_000 + probes {
            if bloom.maybe_contains(make_id(i).as_bytes()) {
                fp += 1;
            }
        }
        // Empirical FPR with k=7, m=10n is ≈0.8%. Bound generously.
        let probes_us = probes as usize;
        assert!(
            fp * 100 < probes_us * 2,
            "fp={fp}/{probes_us} ≈ {:.2}%",
            (fp as f64) * 100.0 / probes_us as f64
        );
    }

    #[test]
    fn rotation_preserves_recent_inserts() {
        let mut bloom = RecentBlocksBloom::new(1_000);
        // Fill exactly to capacity → no rotation yet.
        for i in 0..1_000 {
            bloom.insert(make_id(i).as_bytes());
        }
        // One more insert triggers rotation.
        bloom.insert(make_id(2_000).as_bytes());
        // The just-rotated entries land in `back` and are still
        // queryable.
        for i in 0..1_000 {
            assert!(bloom.maybe_contains(make_id(i).as_bytes()));
        }
        assert!(bloom.maybe_contains(make_id(2_000).as_bytes()));
    }

    #[test]
    fn warmed_flag_starts_false() {
        let mut bloom = RecentBlocksBloom::new(100);
        assert!(!bloom.is_warmed());
        bloom.mark_warmed();
        assert!(bloom.is_warmed());
    }
}
