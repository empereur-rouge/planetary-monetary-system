//! Spent outpoint tracking for double-spend detection.

use super::ConcurrentDag;

impl ConcurrentDag {
    /// Mark an outpoint as spent (bounded FIFO eviction when limit > 0)
    pub fn mark_spent(&self, txid: &str, index: u32) {
        let key = (txid.to_string(), index);
        if self.spent_outpoints.insert(key.clone()) {
            if self.max_spent_outpoints > 0 {
                let mut order = match self.spent_order.lock() {
                    Ok(o) => o,
                    Err(poisoned) => poisoned.into_inner(),
                };
                order.push_back(key);
                while order.len() > self.max_spent_outpoints {
                    if let Some(oldest) = order.pop_front() {
                        self.spent_outpoints.remove(&oldest);
                    }
                }
            }
        }
    }

    /// Check if an outpoint is already spent
    pub fn is_spent(&self, txid: &str, index: u32) -> bool {
        self.spent_outpoints.contains(&(txid.to_string(), index))
    }
}
