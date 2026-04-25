//! Coordinator key rotation history storage (audit item 8, v0.7.4).
//!
//! The DAG accepts blocks signed by the Coordinator's secp256k1 key. To
//! rotate that key without restarting the network we ship the
//! `CoordinatorKeyRotate` payload — once persisted, this CF tracks
//! every applied rotation so the validator can rebuild "the set of
//! still-accepted signer keys" at boot.

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// One row in the rotation history. Persisted as JSON for forward
/// compatibility (new fields can be added with `#[serde(default)]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyRotationRecord {
    /// Old (revoked-after-grace) coordinator public key, hex-encoded.
    pub old_pk: String,
    /// New (effective immediately) coordinator public key, hex-encoded.
    pub new_pk: String,
    /// ID of the DAG block that carried the rotation payload.
    pub applied_at_block_id: String,
    /// Wall-clock time of the rotation, milliseconds since UNIX epoch.
    /// Used to compute when `old_pk`'s grace window expires.
    pub applied_at_ts_ms: i64,
    /// Number of seconds during which `old_pk` is still accepted as a
    /// valid block signer after `applied_at_ts_ms`. `0` means atomic
    /// revocation — `old_pk` is rejected starting from the next block.
    pub grace_window_seconds: u64,
}

impl KeyRotationRecord {
    /// True when `old_pk` is still a valid signer at the given wall
    /// clock. `now_ms` is `i64` to match the rest of the codebase
    /// (block timestamps are stored as i64).
    pub fn old_key_in_grace(&self, now_ms: i64) -> bool {
        if self.grace_window_seconds == 0 {
            return false;
        }
        let elapsed_ms = now_ms.saturating_sub(self.applied_at_ts_ms);
        let grace_ms = self.grace_window_seconds.saturating_mul(1000) as i64;
        elapsed_ms >= 0 && elapsed_ms < grace_ms
    }
}

/// Persistence trait for coordinator key rotations.
///
/// Reads are infrequent (boot-time history replay + on each rotation)
/// so methods are synchronous to keep the call sites simple — the
/// production `RocksStore` impl runs them on the calling thread.
///
/// Default impls make this an opt-in trait: mock backends used in unit
/// tests (and any future read-only replica) inherit a no-op default
/// where rotations are silently dropped and the history is empty. Only
/// `RocksStore` ships a working impl — every persist site that cares
/// about durability hits a `RocksStore`.
pub trait CoordinatorKeyStorage: Send + Sync {
    /// Append a rotation to the history. Idempotent on `applied_at_block_id`
    /// — a second call with the same block ID is a no-op.
    fn record_key_rotation(&self, _rotation: &KeyRotationRecord) -> Result<()> {
        Ok(())
    }

    /// Return every recorded rotation, ordered chronologically (oldest
    /// first). Used at boot to compute the active signer set.
    fn list_key_rotations(&self) -> Result<Vec<KeyRotationRecord>> {
        Ok(Vec::new())
    }
}
