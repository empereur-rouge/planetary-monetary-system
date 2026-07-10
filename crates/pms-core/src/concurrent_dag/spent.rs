//! Spent outpoint tracking for double-spend detection.

use super::ConcurrentDag;
use anyhow::Result;
use pms_storage::DagStorage;

impl ConcurrentDag {
    /// Mark an outpoint as spent (bounded FIFO eviction when limit > 0).
    /// Idempotent; ignores whether it was already spent. For the live persist
    /// path use [`ConcurrentDag::try_mark_spent`] instead, which reports
    /// double-spends.
    pub fn mark_spent(&self, txid: &str, index: u32) {
        let _ = self.try_mark_spent(txid, index);
    }

    /// **Atomic double-spend claim.** Inserts the outpoint into the spent-set and
    /// returns `true` if it was **newly** claimed (this caller owns the spend),
    /// `false` if it was **already** spent. `DashSet::insert` is atomic, so when
    /// concurrent blocks race on the same outpoint exactly one gets `true` — this
    /// is the authoritative commit point that closes the validate→apply TOCTOU on
    /// the live persist path (`do_persist_block_internal`). Validation (which
    /// reads the UTXO set lock-free) is only an early reject; THIS decides.
    pub fn try_mark_spent(&self, txid: &str, index: u32) -> bool {
        let key = (txid.to_string(), index);
        let newly = self.spent_outpoints.insert(key.clone());
        if newly && self.max_spent_outpoints > 0 {
            let mut order = self.spent_order.lock();
            order.push_back(key);
            while order.len() > self.max_spent_outpoints {
                if let Some(oldest) = order.pop_front() {
                    self.spent_outpoints.remove(&oldest);
                }
            }
        }
        newly
    }

    /// Undo a [`ConcurrentDag::try_mark_spent`] claim — used to roll back a
    /// multi-input block that is rejected AFTER claiming some of its inputs, so
    /// legitimately-unspent inputs are not locked up. Removes the key from the
    /// RAM set; a stale `spent_order` entry is harmless (its later FIFO `remove`
    /// is a no-op).
    pub fn unmark_spent(&self, txid: &str, index: u32) {
        self.spent_outpoints.remove(&(txid.to_string(), index));
    }

    /// Best-effort RAM check — **do not use for financial validation alone**.
    ///
    /// The RAM tracker is a bounded FIFO: once `max_spent_outpoints` is
    /// reached, the oldest entries are evicted from memory. An evicted
    /// outpoint is still recorded in the RocksDB `utxo_spent` column
    /// family, so this method will return `false` for outpoints that have
    /// actually been spent long ago.
    ///
    /// For any flow that decides whether to accept a spend, use
    /// [`ConcurrentDag::is_outpoint_spent_authoritative`] instead.
    pub fn is_spent(&self, txid: &str, index: u32) -> bool {
        self.spent_outpoints.contains(&(txid.to_string(), index))
    }

    /// Authoritative double-spend check: RAM fast-path + storage fallback.
    ///
    /// Returns `Ok(true)` if the outpoint is spent according to either the
    /// in-memory tracker or the persistent `utxo_spent` column family. Use
    /// this on any validation path that must not be fooled by FIFO
    /// eviction of the RAM tracker.
    pub async fn is_outpoint_spent_authoritative<S: DagStorage + ?Sized>(
        &self,
        store: &S,
        txid: &str,
        index: u32,
    ) -> Result<bool> {
        if self.is_spent(txid, index) {
            return Ok(true);
        }
        store.is_outpoint_spent(txid, index).await
    }

    /// **Atomic bridge-mint anti-replay claim.** A `BridgeMint` creates funds on
    /// this ledger backed by a `BridgeLock` on the source ledger; each source
    /// lock may be minted AT MOST ONCE. Inserts `lock_block_id` into the
    /// consumed-set and returns `true` if it was **newly** claimed (this mint
    /// owns the lock), `false` if it was **already** consumed (a replay).
    /// `DashSet::insert` is atomic, so two concurrent replays of the same lock
    /// resolve to exactly one winner — the in-process commit point that mirrors
    /// [`ConcurrentDag::try_mark_spent`] for double-spends. The cross-restart
    /// record lives in the durable `bridge_consumed` column family; the early
    /// reject for already-persisted replays is
    /// [`DagStorage::is_bridge_lock_consumed`] (audit rang 3, B3).
    pub fn try_consume_bridge_lock(&self, lock_block_id: &str) -> bool {
        self.consumed_bridge_locks.insert(lock_block_id.to_string())
    }

    /// Undo a [`ConcurrentDag::try_consume_bridge_lock`] claim. Reserved for
    /// symmetry with [`ConcurrentDag::unmark_spent`]: there is **no current
    /// caller** because a BridgeMint claims exactly one lock and has no
    /// post-claim reject path (its UTXO delta has no `spend`, so the
    /// double-spend guard is a no-op for it). Kept as cheap insurance for any
    /// future multi-claim bridge flow that could reject after claiming.
    pub fn unconsume_bridge_lock(&self, lock_block_id: &str) {
        self.consumed_bridge_locks.remove(lock_block_id);
    }

    /// RAM fast-path check for bridge-lock consumption. Pair with the durable
    /// [`DagStorage::is_bridge_lock_consumed`] for an authoritative answer — the
    /// RAM set is empty after a restart until each lock is re-claimed, so it
    /// only covers the in-flight window before the durable batch write lands.
    pub fn is_bridge_lock_consumed_ram(&self, lock_block_id: &str) -> bool {
        self.consumed_bridge_locks.contains(lock_block_id)
    }

    /// **Atomic custodial-mint anti-replay claim** (protocole 2.8). A
    /// `CustodialMint` creates fresh units of a custom asset authorized by the
    /// `mint_authority` signature; each `(asset_id, mint_nonce)` — `key` here,
    /// built by `custodial_mint_consumed_key` — may be minted AT MOST ONCE.
    /// Inserts `key` and returns `true` if **newly** claimed, `false` if it was
    /// **already** consumed (a replay). `DashSet::insert` is atomic → two
    /// concurrent submissions of the same signed payload resolve to exactly one
    /// winner (same commit-point discipline as [`ConcurrentDag::try_mark_spent`]
    /// and [`ConcurrentDag::try_consume_bridge_lock`]). The cross-restart record
    /// is the durable `custodial_mint_consumed` column family; the early reject is
    /// [`DagStorage::is_custodial_mint_consumed`].
    pub fn try_consume_custodial_mint(&self, key: &str) -> bool {
        self.consumed_custodial_mints.insert(key.to_string())
    }

    /// Undo a [`ConcurrentDag::try_consume_custodial_mint`] claim. Used if the
    /// block is rejected AFTER the claim (e.g. a later commit-point guard fails),
    /// so a transient failure doesn't permanently burn the nonce.
    pub fn unconsume_custodial_mint(&self, key: &str) {
        self.consumed_custodial_mints.remove(key);
    }

    /// RAM fast-path check for custodial-mint consumption. Pair with the durable
    /// [`DagStorage::is_custodial_mint_consumed`] for an authoritative answer —
    /// the RAM set is empty after a restart until each nonce is re-claimed, so it
    /// only covers the in-flight window before the durable batch write lands.
    pub fn is_custodial_mint_consumed_ram(&self, key: &str) -> bool {
        self.consumed_custodial_mints.contains(key)
    }

    /// **Atomic collection-ownership claim** for SFT anti-squat (protocole 2.8,
    /// Q4). The first `SftClassCreate` referencing `collection_id` claims it for
    /// `owner`; later classes under the same collection must present the same
    /// owner. Returns `Ok(())` if the claim is consistent (fresh claim, or an
    /// existing claim by the SAME owner), `Err(existing_owner)` if the collection
    /// is already owned by a DIFFERENT party (squat attempt). `DashMap::entry` is
    /// atomic, so concurrent first-claims resolve to exactly one owner. The
    /// cross-restart record is the durable `sft_collections` column family.
    pub fn try_claim_collection(&self, collection_id: &str, owner: &str) -> Result<(), String> {
        use dashmap::mapref::entry::Entry;
        match self.claimed_collections.entry(collection_id.to_string()) {
            Entry::Occupied(e) => {
                let existing = e.get();
                if existing.eq_ignore_ascii_case(owner) {
                    Ok(())
                } else {
                    Err(existing.clone())
                }
            }
            Entry::Vacant(v) => {
                v.insert(owner.to_string());
                Ok(())
            }
        }
    }
}
