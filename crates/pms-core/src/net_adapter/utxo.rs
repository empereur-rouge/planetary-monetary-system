//! UTXO query and mutation helpers for the `NetDagAdapter` implementation.
//!
//! Provides `utxos_by_address()`, `utxos_for_selection()`, `add_utxo()`,
//! `remove_utxo()`, and `get_utxo()` as `pub(super)` methods on
//! `CoreAdapter` so the trait impl in `mod.rs` can delegate to them.

use crate::CoreAdapter;
use pms_storage::coordinator_key_store::CoordinatorKeyStorage;
use pms_storage::{ComplianceStorage, ConfigStorage, DagStorage, NftStorage, NodeRewardsStorage};
use pms_types::{OutputId, TxOutput};

impl<S> CoreAdapter<S>
where
    S: DagStorage
        + NftStorage
        + ConfigStorage
        + NodeRewardsStorage
        + ComplianceStorage
        + CoordinatorKeyStorage
        + Send
        + Sync
        + 'static,
{
    /// All unspent outputs belonging to the given address.
    pub(super) async fn do_utxos_by_address(
        &self,
        address: &str,
    ) -> Vec<(OutputId, TxOutput)> {
        self.utxos.utxos_by_address(address).await
    }

    /// Fast-path coin selection: returns UTXOs up to `limit` that cover `target`.
    pub(super) async fn do_utxos_for_selection(
        &self,
        address: &str,
        asset_id: &Option<String>,
        target: rust_decimal::Decimal,
        limit: usize,
    ) -> (
        Vec<(OutputId, TxOutput, rust_decimal::Decimal)>,
        rust_decimal::Decimal,
    ) {
        self.utxos
            .utxos_by_address_for_selection(address, asset_id, target, limit)
            .await
    }

    /// Insert a new UTXO into the sharded set.
    /// Le `TxOutput` complet est inséré tel quel — aucun champ protocole
    /// (asset_id, locked_until, spend_condition, …) n'est reconstruit à la main.
    pub(super) async fn do_add_utxo(&self, txid: String, index: u32, output: TxOutput) {
        self.utxos.add(OutputId { txid, index }, output).await;
    }

    /// Remove a UTXO by its output id. Returns `true` if it existed.
    pub(super) async fn do_remove_utxo(&self, output_id: &OutputId) -> bool {
        self.utxos.remove(output_id).await.is_some()
    }

    /// Look up a single UTXO by output id.
    pub(super) async fn do_get_utxo(&self, output_id: &OutputId) -> Option<TxOutput> {
        self.utxos.get(output_id).await
    }
}
