//! Finality checking for ConcurrentDag.

use super::ConcurrentDag;

impl ConcurrentDag {
    /// Check if a block is final (helper)
    pub fn is_final(&self, block_id: &str) -> bool {
        match self.finality.read() {
            Ok(f) => f.finalized.contains(block_id),
            Err(poisoned) => poisoned.into_inner().finalized.contains(block_id),
        }
    }
}
