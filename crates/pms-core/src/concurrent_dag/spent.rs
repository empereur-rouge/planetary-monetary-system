//! Spent outpoint tracking for double-spend detection.

use super::ConcurrentDag;
use anyhow::Result;
use pms_storage::DagStorage;

impl ConcurrentDag {
    /// Mark an outpoint as spent (bounded FIFO eviction when limit > 0)
    pub fn mark_spent(&self, txid: &str, index: u32) {
        let key = (txid.to_string(), index);
        if self.spent_outpoints.insert(key.clone()) && self.max_spent_outpoints > 0 {
            let mut order = self.spent_order.lock();
            order.push_back(key);
            while order.len() > self.max_spent_outpoints {
                if let Some(oldest) = order.pop_front() {
                    self.spent_outpoints.remove(&oldest);
                }
            }
        }
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
}
