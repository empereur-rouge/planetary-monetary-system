//! `NetDagAdapter` implementation for [`CoreAdapter`].
//!
//! The actual logic is split across sub-modules for readability:
//!
//! | Module | Content |
//! |--------|---------|
//! | [`persist`] | `do_persist_block()` -- validation -> persistence -> UTXO pipeline |
//! | [`query`]   | `do_have_block()`, `do_get_block()`, `do_recent_ids()`, `do_top_tips()` |
//! | [`supply`]  | `do_circulating_supply*()`, `do_balance_by_address*()` |
//! | [`utxo`]    | `do_utxos_by_address()`, `do_utxos_for_selection()`, `do_add_utxo()` etc. |
//! | [`helpers`] | `plain_payload_type_str()` |
//!
//! This file contains only the thin `impl NetDagAdapter for CoreAdapter<S>`
//! that delegates each method to the corresponding `do_*` helper.

mod helpers;
mod persist;
mod query;
mod supply;
mod utxo;

use crate::CoreAdapter;
use anyhow::Result;
use async_trait::async_trait;
use pms_interface::NetDagAdapter;
use pms_storage::store::PutResult;

use pms_wire::WireBlock;

#[async_trait]
impl<S> NetDagAdapter for CoreAdapter<S>
where
    S: pms_storage::EngineStorage,
{
    async fn have_block(&self, id: &str) -> bool {
        self.do_have_block(id).await
    }

    #[allow(clippy::too_many_lines)]
    async fn persist_block(&self, wb: &WireBlock) -> Result<PutResult> {
        self.do_persist_block_internal(wb, None).await
    }

    async fn persist_block_with_delta(
        &self,
        wb: &WireBlock,
        delta: pms_storage::UtxoDelta,
    ) -> Result<PutResult> {
        self.do_persist_block_internal(wb, Some(delta)).await
    }

    /// Override (vs le défaut fail-closed du trait) : valide le plaintext d'un
    /// `TxUtxo` via la MÊME logique que le hot-path (`validate_plain_txutxo`),
    /// pour que les handlers de payload chiffré appliquent exactement les mêmes
    /// contrôles. Utilise `self.policy` (base) — l'override de rotation ne touche
    /// que `coordinator_public_key`, non lu par la validation TxUtxo.
    async fn validate_txutxo_full(
        &self,
        tx: &pms_types::Transaction,
        now_ms: u64,
    ) -> std::result::Result<Vec<pms_types::TxOutput>, String> {
        self.validate_plain_txutxo(tx, &self.policy, now_ms).await
    }

    async fn broadcast_block(&self, wb: &WireBlock) -> Result<()> {
        self.do_broadcast_block(wb).await
    }

    fn persist_queue_depth(&self) -> Option<(usize, usize)> {
        let max = self.persist_tx.max_capacity();
        let avail = self.persist_tx.capacity();
        Some((max.saturating_sub(avail), max))
    }

    async fn utxo_set_size(&self) -> Option<usize> {
        Some(self.utxos.total_len().await)
    }

    async fn top_tips(&self, limit: usize) -> Result<Vec<String>> {
        self.do_top_tips(limit).await
    }

    async fn get_block(&self, id: &str) -> Result<Option<WireBlock>> {
        self.do_get_block(id).await
    }

    async fn recent_ids(&self, limit: usize) -> Result<Vec<String>> {
        self.do_recent_ids(limit).await
    }

    async fn get_blocks_by_ids(&self, ids: &[String]) -> Result<Vec<WireBlock>> {
        self.do_get_blocks_by_ids(ids).await
    }

    async fn tip_count_estimate(&self) -> usize {
        self.store.tip_count_estimate().await
    }

    async fn count_descendants(&self, block_id: &str, max_count: usize) -> usize {
        self.dag.count_descendants(block_id, max_count)
    }

    async fn is_finalized(&self, block_id: &str) -> bool {
        self.dag.is_final(block_id)
    }

    async fn last_milestone(&self) -> Option<String> {
        self.dag.finality.read().last_milestone.clone()
    }

    fn min_pow_leading_zero_bits(&self) -> u8 {
        self.policy.min_pow_leading_zero_bits
    }

    async fn circulating_supply(&self) -> (rust_decimal::Decimal, u64) {
        self.do_circulating_supply().await
    }

    async fn circulating_supply_by_asset(
        &self,
        asset_id: Option<&str>,
    ) -> (rust_decimal::Decimal, u64) {
        self.do_circulating_supply_by_asset(asset_id).await
    }

    async fn balance_by_address(&self, address: &str) -> rust_decimal::Decimal {
        self.do_balance_by_address(address).await
    }

    async fn balance_by_address_and_asset(
        &self,
        address: &str,
        asset_id: Option<&str>,
    ) -> rust_decimal::Decimal {
        self.do_balance_by_address_and_asset(address, asset_id).await
    }

    async fn utxos_by_address(
        &self,
        address: &str,
    ) -> Vec<(pms_types::OutputId, pms_types::TxOutput)> {
        self.do_utxos_by_address(address).await
    }

    async fn utxos_for_selection(
        &self,
        address: &str,
        asset_id: &Option<String>,
        target: rust_decimal::Decimal,
        limit: usize,
    ) -> (
        Vec<(pms_types::OutputId, pms_types::TxOutput, rust_decimal::Decimal)>,
        rust_decimal::Decimal,
    ) {
        self.do_utxos_for_selection(address, asset_id, target, limit)
            .await
    }

    async fn add_utxo(&self, txid: String, index: u32, output: pms_types::TxOutput) {
        self.do_add_utxo(txid, index, output).await;
    }

    async fn remove_utxo(&self, output_id: &pms_types::OutputId) -> bool {
        self.do_remove_utxo(output_id).await
    }

    async fn get_utxo(&self, output_id: &pms_types::OutputId) -> Option<pms_types::TxOutput> {
        self.do_get_utxo(output_id).await
    }

    fn event_bus(&self) -> Option<pms_event::EventBus> {
        Some(self.event_bus.clone())
    }
}
