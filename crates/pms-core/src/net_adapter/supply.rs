//! Supply and balance query helpers for the `NetDagAdapter` implementation.
//!
//! Provides `circulating_supply*()` and `balance_by_address*()` as
//! `pub(super)` methods on `CoreAdapter` so the trait impl in `mod.rs`
//! can delegate to them.

use crate::CoreAdapter;
use pms_storage::coordinator_key_store::CoordinatorKeyStorage;
use pms_storage::{ComplianceStorage, ConfigStorage, DagStorage, NftStorage, NodeRewardsStorage};

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
    /// Total circulating supply of the native PMS token.
    pub(super) async fn do_circulating_supply(&self) -> (rust_decimal::Decimal, u64) {
        let (dec, count) = self.utxos.circulating_supply().await;
        (dec, count as u64)
    }

    /// Circulating supply filtered by optional asset id.
    pub(super) async fn do_circulating_supply_by_asset(
        &self,
        asset_id: Option<&str>,
    ) -> (rust_decimal::Decimal, u64) {
        let (dec, count) = self.utxos.circulating_supply_by_asset(asset_id).await;
        (dec, count as u64)
    }

    /// Balance of a single address (native PMS token only).
    pub(super) async fn do_balance_by_address(&self, address: &str) -> rust_decimal::Decimal {
        self.utxos.balance_by_address(address).await
    }

    /// Balance of a single address, optionally filtered by asset id.
    pub(super) async fn do_balance_by_address_and_asset(
        &self,
        address: &str,
        asset_id: Option<&str>,
    ) -> rust_decimal::Decimal {
        self.utxos
            .balance_by_address_and_asset(address, asset_id)
            .await
    }
}
