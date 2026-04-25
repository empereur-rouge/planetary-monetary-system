//! RocksDB implementation of [`CoordinatorKeyStorage`] (audit item 8, v0.7.4).
//!
//! Persists rotation history in the `coordinator_key_history` CF. Keys
//! are big-endian timestamp + block-id so a forward scan returns the
//! rotations in chronological order — the validator's boot replay
//! relies on that ordering to compute the active signer set correctly.

use crate::coordinator_key_store::{CoordinatorKeyStorage, KeyRotationRecord};
use crate::rocks_store::store::RocksStore;
use anyhow::Result;

impl RocksStore {
    /// Build the composite key `[ts_ms_be:8][0x00][block_id]`.
    /// Big-endian on the timestamp so RocksDB's lexicographic ordering
    /// matches chronological order.
    fn coord_key_history_key(rotation: &KeyRotationRecord) -> Vec<u8> {
        let mut key = Vec::with_capacity(8 + 1 + rotation.applied_at_block_id.len());
        key.extend_from_slice(&(rotation.applied_at_ts_ms as u64).to_be_bytes());
        key.push(0);
        key.extend_from_slice(rotation.applied_at_block_id.as_bytes());
        key
    }
}

impl CoordinatorKeyStorage for RocksStore {
    fn record_key_rotation(&self, rotation: &KeyRotationRecord) -> Result<()> {
        let cf = self.cf("coordinator_key_history");
        let key = Self::coord_key_history_key(rotation);

        // Idempotency: if a row already exists for this exact composite
        // key, we skip rewriting it. The block ID is derived from the
        // payload so the same rotation block always produces the same
        // key — this protects against replay during reorg or migration.
        if self.db.get_cf(&cf, &key)?.is_some() {
            return Ok(());
        }
        let value = serde_json::to_vec(rotation)?;
        self.db.put_cf(&cf, &key, value)?;
        Ok(())
    }

    fn list_key_rotations(&self) -> Result<Vec<KeyRotationRecord>> {
        let cf = self.cf("coordinator_key_history");
        let mut out = Vec::new();
        for kv in self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start) {
            let (_k, v) = kv?;
            let record: KeyRotationRecord = serde_json::from_slice(&v)?;
            out.push(record);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    //! Storage-layer tests for the rotation history. Validator-level
    //! integration tests live in `crates/pms-core/tests/coordinator_key_rotation.rs`.

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

    fn rec(old: &str, new: &str, block_id: &str, ts_ms: i64, grace: u64) -> KeyRotationRecord {
        KeyRotationRecord {
            old_pk: old.into(),
            new_pk: new.into(),
            applied_at_block_id: block_id.into(),
            applied_at_ts_ms: ts_ms,
            grace_window_seconds: grace,
        }
    }

    #[tokio::test]
    async fn record_and_list_round_trip() {
        let (store, _dir) = fresh_store().await;

        let r1 = rec("pk-A", "pk-B", "block-1", 1_000, 60);
        let r2 = rec("pk-B", "pk-C", "block-2", 5_000, 0);

        store.record_key_rotation(&r1).expect("record r1");
        store.record_key_rotation(&r2).expect("record r2");

        let listed = store.list_key_rotations().expect("list");
        println!("listed rotations: {listed:?}");
        assert_eq!(listed.len(), 2);
        // Chronological order — r1 (ts=1000) before r2 (ts=5000)
        assert_eq!(listed[0], r1);
        assert_eq!(listed[1], r2);
    }

    #[tokio::test]
    async fn record_is_idempotent_on_same_block() {
        let (store, _dir) = fresh_store().await;
        let r = rec("pk-A", "pk-B", "same-block", 1_000, 60);

        store.record_key_rotation(&r).unwrap();
        store.record_key_rotation(&r).unwrap(); // second call must no-op

        let listed = store.list_key_rotations().unwrap();
        println!("after double-record: {} entries", listed.len());
        assert_eq!(listed.len(), 1);
    }

    #[test]
    fn old_key_in_grace_boundary() {
        let r = rec("pk-A", "pk-B", "blk", 10_000, 60);
        // Within grace
        assert!(r.old_key_in_grace(10_000), "exact moment of rotation");
        assert!(r.old_key_in_grace(10_000 + 30_000), "midway through grace");
        // Past grace
        assert!(!r.old_key_in_grace(10_000 + 60_001), "1ms after grace ends");
        // Negative skew (clock went backwards) — treat as not-in-grace,
        // never accept a key from the future.
        assert!(!r.old_key_in_grace(9_000), "wall-clock before rotation ts");
        // grace_window_seconds = 0 → revocation is atomic, never in grace.
        let atomic = rec("pk-A", "pk-B", "blk", 10_000, 0);
        assert!(!atomic.old_key_in_grace(10_000));
    }
}
