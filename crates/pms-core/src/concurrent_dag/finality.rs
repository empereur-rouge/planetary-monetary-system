//! Finality checking for ConcurrentDag.

use super::ConcurrentDag;

impl ConcurrentDag {
    /// Check if a block is final (helper)
    pub fn is_final(&self, block_id: &str) -> bool {
        self.finality.read().finalized.contains(block_id)
    }
}
