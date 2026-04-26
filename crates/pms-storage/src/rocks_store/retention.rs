//! Retention helpers: scan-and-delete entries older than a wall-clock
//! cutoff in the activity and compliance-log CFs (audit follow-up,
//! v0.7.4 post-launch).
//!
//! Why this exists. `addr_activity`, `addr_type_activity`, and
//! `activity_items` grow O(blocks × addresses-per-block) and never
//! shrink — under sustained traffic the per-address indexes can
//! dominate disk usage even though the underlying blocks have already
//! been pruned from the DAG. `compliance_log` grows linearly with
//! freeze/seize/reverse operations and is regulatory data that the
//! operator may legally need to keep for years before archiving.
//!
//! Two scopes:
//!   * Activity CFs are a fast-path cache derived from the blocks
//!     themselves. Safe to purge automatically on a retention window
//!     (config: `[health].activity_retention_days`).
//!   * Compliance log is audit material. We expose a manual purge
//!     helper but never run it automatically — the operator decides
//!     when to archive and prune.

use crate::rocks_store::store::RocksStore;
use anyhow::Result;
use rocksdb::{IteratorMode, WriteBatch};
use serde::{Deserialize, Serialize};

/// Number of deletes per `WriteBatch::write()` flush. Chosen to keep
/// each batch around 64–128 KB on the wire — large enough that WAL
/// amortisation matters, small enough that a long scan doesn't pin
/// memory.
const PURGE_BATCH_SIZE: usize = 1_000;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ActivityPurgeStats {
    /// Total entries scanned across the three CFs.
    pub scanned: u64,
    /// Entries deleted because their embedded timestamp was strictly
    /// less than the cutoff.
    pub deleted: u64,
    /// Cutoff timestamp this run used (echoed back so audit logs are
    /// self-contained).
    pub cutoff_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ComplianceLogPurgeStats {
    pub scanned: u64,
    pub deleted: u64,
    pub cutoff_ms: i64,
}

/// Extract the embedded `ts_ms` from an `addr_activity`-shaped key
/// without needing to know the address length up front. The key format
/// is `[addr:N][0x00][ts_be:8][...]` and addresses are bech32 (printable
/// ASCII, no NUL), so the first 0x00 byte is unambiguously the
/// separator.
///
/// Returns `None` if the key is malformed (no separator, or the
/// timestamp slice is too short) — those entries are skipped rather
/// than deleted, since silently dropping rows we can't parse would be
/// worse than leaving a couple of zombies behind.
fn extract_ts_from_addr_key(key: &[u8]) -> Option<i64> {
    let sep = key.iter().position(|&b| b == 0)?;
    let ts_start = sep + 1;
    if key.len() < ts_start + 8 {
        return None;
    }
    let mut ts_be = [0u8; 8];
    ts_be.copy_from_slice(&key[ts_start..ts_start + 8]);
    Some(u64::from_be_bytes(ts_be) as i64)
}

/// Like `extract_ts_from_addr_key` but for the `addr_type_activity` CF
/// where the layout is `[addr:N][0x00][cat:1][ts_be:8][...]`. Only one
/// extra byte to skip vs the activity key.
fn extract_ts_from_addr_type_key(key: &[u8]) -> Option<i64> {
    let sep = key.iter().position(|&b| b == 0)?;
    let ts_start = sep + 2; // +1 for the 0x00 sep, +1 for the cat byte
    if key.len() < ts_start + 8 {
        return None;
    }
    let mut ts_be = [0u8; 8];
    ts_be.copy_from_slice(&key[ts_start..ts_start + 8]);
    Some(u64::from_be_bytes(ts_be) as i64)
}

impl RocksStore {
    /// Delete every entry in the activity CFs whose embedded timestamp
    /// is strictly less than `cutoff_ms`. The three CFs are scanned
    /// independently and deletes are batched via `WriteBatch` for WAL
    /// amortisation.
    ///
    /// Returns counts so the caller can log / surface the work done.
    /// Failures inside the loop are bubbled up — partial progress is
    /// still committed because each batch is written before the next
    /// scan iteration.
    pub fn purge_activity_before(&self, cutoff_ms: i64) -> Result<ActivityPurgeStats> {
        let mut stats = ActivityPurgeStats {
            scanned: 0,
            deleted: 0,
            cutoff_ms,
        };

        // ---- addr_activity ----
        {
            let cf = self.cf("addr_activity");
            let mut batch = WriteBatch::default();
            let mut in_batch = 0usize;
            for kv in self.db.iterator_cf(&cf, IteratorMode::Start) {
                let (k, _v) = kv?;
                stats.scanned += 1;
                let Some(ts) = extract_ts_from_addr_key(&k) else {
                    continue;
                };
                if ts < cutoff_ms {
                    batch.delete_cf(&cf, &k);
                    in_batch += 1;
                    if in_batch >= PURGE_BATCH_SIZE {
                        self.db.write(std::mem::take(&mut batch))?;
                        stats.deleted += in_batch as u64;
                        in_batch = 0;
                    }
                }
            }
            if in_batch > 0 {
                self.db.write(batch)?;
                stats.deleted += in_batch as u64;
            }
        }

        // ---- addr_type_activity ----
        {
            let cf = self.cf("addr_type_activity");
            let mut batch = WriteBatch::default();
            let mut in_batch = 0usize;
            for kv in self.db.iterator_cf(&cf, IteratorMode::Start) {
                let (k, _v) = kv?;
                stats.scanned += 1;
                let Some(ts) = extract_ts_from_addr_type_key(&k) else {
                    continue;
                };
                if ts < cutoff_ms {
                    batch.delete_cf(&cf, &k);
                    in_batch += 1;
                    if in_batch >= PURGE_BATCH_SIZE {
                        self.db.write(std::mem::take(&mut batch))?;
                        stats.deleted += in_batch as u64;
                        in_batch = 0;
                    }
                }
            }
            if in_batch > 0 {
                self.db.write(batch)?;
                stats.deleted += in_batch as u64;
            }
        }

        // ---- activity_items ----
        // Same key layout as addr_activity (`[addr][0x00][ts:8][...]`).
        {
            let cf = self.cf("activity_items");
            let mut batch = WriteBatch::default();
            let mut in_batch = 0usize;
            for kv in self.db.iterator_cf(&cf, IteratorMode::Start) {
                let (k, _v) = kv?;
                stats.scanned += 1;
                let Some(ts) = extract_ts_from_addr_key(&k) else {
                    continue;
                };
                if ts < cutoff_ms {
                    batch.delete_cf(&cf, &k);
                    in_batch += 1;
                    if in_batch >= PURGE_BATCH_SIZE {
                        self.db.write(std::mem::take(&mut batch))?;
                        stats.deleted += in_batch as u64;
                        in_batch = 0;
                    }
                }
            }
            if in_batch > 0 {
                self.db.write(batch)?;
                stats.deleted += in_batch as u64;
            }
        }

        tracing::info!(
            target = "activity_retention",
            cutoff_ms,
            scanned = stats.scanned,
            deleted = stats.deleted,
            "purge_activity_before completed"
        );
        Ok(stats)
    }

    /// Delete every `compliance_log` entry whose `timestamp_ms` is
    /// strictly less than `cutoff_ms`. The CF is keyed by `block_id`
    /// (no embedded timestamp), so we deserialize each value's JSON
    /// and check `timestamp_ms` from there. Slower than the activity
    /// CFs (one JSON parse per row) but compliance volumes are tiny
    /// in practice.
    ///
    /// CALLER WARNING: this is regulatory audit material. Do not run
    /// it automatically; require an explicit operator action with a
    /// specific cutoff. The whole point of the log is that it
    /// outlives day-to-day operations.
    pub fn purge_compliance_log_before(
        &self,
        cutoff_ms: i64,
    ) -> Result<ComplianceLogPurgeStats> {
        use crate::rocks_store::compliance_registry::ComplianceLogEntry;

        let cf = self.cf("compliance_log");
        let mut stats = ComplianceLogPurgeStats {
            scanned: 0,
            deleted: 0,
            cutoff_ms,
        };
        let mut batch = WriteBatch::default();
        let mut in_batch = 0usize;

        for kv in self.db.iterator_cf(&cf, IteratorMode::Start) {
            let (k, v) = kv?;
            stats.scanned += 1;
            let entry: ComplianceLogEntry = match serde_json::from_slice(&v) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(
                        target = "compliance_retention",
                        error = %e,
                        "skipping un-deserialisable compliance_log entry — leaving in place"
                    );
                    continue;
                }
            };
            if entry.timestamp_ms < cutoff_ms {
                batch.delete_cf(&cf, &k);
                in_batch += 1;
                if in_batch >= PURGE_BATCH_SIZE {
                    self.db.write(std::mem::take(&mut batch))?;
                    stats.deleted += in_batch as u64;
                    in_batch = 0;
                }
            }
        }
        if in_batch > 0 {
            self.db.write(batch)?;
            stats.deleted += in_batch as u64;
        }

        tracing::warn!(
            target = "compliance_retention",
            cutoff_ms,
            scanned = stats.scanned,
            deleted = stats.deleted,
            "purge_compliance_log_before completed — REGULATORY DATA REMOVED"
        );
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rocks_store::store::RocksMemoryConfig;
    use tempfile::TempDir;

    async fn fresh_store() -> (RocksStore, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().to_string_lossy().into_owned();
        let store = RocksStore::new(&path, 16, "main", None, &RocksMemoryConfig::default())
            .await
            .expect("rocks");
        (store, dir)
    }

    fn write_addr_activity(store: &RocksStore, addr: &str, ts_ms: i64, block_id: &str) {
        let key = crate::helpers::key_addr_activity(addr, ts_ms, block_id);
        let cf = store.cf("addr_activity");
        store.db.put_cf(&cf, &key, b"").unwrap();
    }

    fn write_addr_type_activity(
        store: &RocksStore,
        addr: &str,
        cat: u8,
        ts_ms: i64,
        block_id: &str,
    ) {
        let key = crate::helpers::key_addr_type_activity(addr, cat, ts_ms, block_id);
        let cf = store.cf("addr_type_activity");
        store.db.put_cf(&cf, &key, b"").unwrap();
    }

    fn write_activity_items(store: &RocksStore, addr: &str, ts_ms: i64, block_id: &str) {
        let key = crate::helpers::key_addr_activity(addr, ts_ms, block_id);
        let cf = store.cf("activity_items");
        store.db.put_cf(&cf, &key, br#"[]"#).unwrap();
    }

    fn count_cf(store: &RocksStore, cf_name: &str) -> usize {
        let cf = store.cf(cf_name);
        store
            .db
            .iterator_cf(&cf, IteratorMode::Start)
            .count()
    }

    #[tokio::test]
    async fn purge_activity_deletes_only_entries_older_than_cutoff() {
        let (store, _dir) = fresh_store().await;

        // Populate three CFs with entries at three timestamps.
        for &ts in &[1_000i64, 5_000, 10_000] {
            write_addr_activity(&store, "addr1", ts, "blk-a");
            write_addr_activity(&store, "addr2", ts, "blk-b");
            write_addr_type_activity(&store, "addr1", 0, ts, "blk-a");
            write_activity_items(&store, "addr1", ts, "blk-a");
        }

        // Sanity: each CF starts with 3 entries (per address, per ts).
        assert_eq!(count_cf(&store, "addr_activity"), 6); // 2 addrs × 3 ts
        assert_eq!(count_cf(&store, "addr_type_activity"), 3);
        assert_eq!(count_cf(&store, "activity_items"), 3);

        // Cutoff = 6_000 → ts ∈ {1_000, 5_000} should die, ts=10_000 lives.
        let stats = store
            .purge_activity_before(6_000)
            .expect("purge_activity_before");
        println!("stats = {stats:?}");

        assert_eq!(stats.cutoff_ms, 6_000);
        // Each CF had 2 entries < cutoff; sum across the 3 CFs:
        //   addr_activity: 2 addrs × 2 ts = 4 deletes
        //   addr_type_activity: 1 addr × 2 ts = 2 deletes
        //   activity_items: 1 addr × 2 ts = 2 deletes
        assert_eq!(stats.deleted, 4 + 2 + 2);

        assert_eq!(count_cf(&store, "addr_activity"), 2); // both addrs at ts=10_000
        assert_eq!(count_cf(&store, "addr_type_activity"), 1);
        assert_eq!(count_cf(&store, "activity_items"), 1);
    }

    #[tokio::test]
    async fn purge_activity_idempotent() {
        let (store, _dir) = fresh_store().await;
        write_addr_activity(&store, "addr1", 1_000, "blk-a");

        let s1 = store.purge_activity_before(2_000).unwrap();
        let s2 = store.purge_activity_before(2_000).unwrap();
        println!("s1={s1:?}, s2={s2:?}");

        assert_eq!(s1.deleted, 1);
        assert_eq!(s2.deleted, 0); // already gone
        assert_eq!(count_cf(&store, "addr_activity"), 0);
    }

    #[tokio::test]
    async fn purge_compliance_log_deletes_only_old_entries() {
        let (store, _dir) = fresh_store().await;

        // Manually populate the CF with three entries at three timestamps.
        let cf = store.cf("compliance_log");
        for (ts, block_id) in [(1_000i64, "blk-1"), (5_000, "blk-2"), (10_000, "blk-3")] {
            let entry = serde_json::json!({
                "action": "freeze",
                "block_id": block_id,
                "target_address": "addr1",
                "details": {},
                "timestamp_ms": ts,
            });
            store
                .db
                .put_cf(&cf, block_id.as_bytes(), serde_json::to_vec(&entry).unwrap())
                .unwrap();
        }
        assert_eq!(count_cf(&store, "compliance_log"), 3);

        let stats = store
            .purge_compliance_log_before(6_000)
            .expect("purge_compliance_log_before");
        println!("stats = {stats:?}");

        assert_eq!(stats.scanned, 3);
        assert_eq!(stats.deleted, 2); // ts=1_000 and 5_000
        assert_eq!(count_cf(&store, "compliance_log"), 1);

        // The surviving row is the one at ts=10_000.
        let kv = store
            .db
            .iterator_cf(&cf, IteratorMode::Start)
            .next()
            .unwrap()
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&kv.1).unwrap();
        assert_eq!(parsed["block_id"], "blk-3");
    }

    #[test]
    fn extract_ts_handles_short_keys_gracefully() {
        assert_eq!(extract_ts_from_addr_key(&[]), None);
        assert_eq!(extract_ts_from_addr_key(b"addrwithoutsep"), None);
        // sep present but ts truncated
        let mut k = b"addr".to_vec();
        k.push(0);
        k.extend_from_slice(&[1u8, 2, 3]);
        assert_eq!(extract_ts_from_addr_key(&k), None);
    }
}
