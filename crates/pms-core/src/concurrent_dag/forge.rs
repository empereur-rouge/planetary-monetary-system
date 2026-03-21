//! Block forging (local block creation) for ConcurrentDag.

use super::ConcurrentDag;
use crate::block_builder::BlockMineBuilder;
use anyhow::Result;
use pms_types::{Block, BlockId, PayloadEnvelope};

impl ConcurrentDag {
    /// Forge a block locally (select parents, build, insert)
    /// This replaces the legacy `Dag::add_payload_auto_parents_mined`
    pub fn forge_block(
        &self,
        payload: Option<PayloadEnvelope>,
        difficulty_leading_zeros: u8,
        compute_id: impl Fn(&[String], &Option<PayloadEnvelope>, u64) -> BlockId,
    ) -> Result<Block> {
        // 1. Select parents (2 min, 2 max)
        let parents = self.select_parents(2, 2);

        // Bootstrapping: if we have blocks but select_parents returns nothing,
        // it means something is wrong unless we only have genesis.
        if parents.is_empty() && self.blocks.len() > 1 {
            return Err(anyhow::anyhow!("No parents available"));
        }

        // 2. Build block
        // Note: BlockMineBuilder uses a callback to check parent existence
        let block =
            BlockMineBuilder::new(parents, payload, compute_id, |id| self.contains_block(id))
                .difficulty(difficulty_leading_zeros)
                .canonicalize_parents(true)
                .build();

        // 3. Insert
        self.insert_block(block.clone());

        // 4. Update finality?
        // Skipped for now in ConcurrentDag as it uses background updates.

        Ok(block)
    }
}
