//! Stockage des gas pools par ledger (anti-spam pour ledgers custom).
//!
//! Ce module définit le trait `GasPoolStorage` pour le CRUD des gas pools
//! qui contrôlent les dépenses de gas par transaction sur les ledgers custom.
//!
//! ## Modèle de données
//! - `ledger_id` → `GasPool` (JSON sérialisé)
//! - Opérations atomiques: deposit, withdraw, consume

use anyhow::Result;
use pms_types_economics::GasPool;
use rust_decimal::Decimal;

/// Trait pour le stockage des gas pools par ledger.
///
/// Chaque ledger custom possède un gas pool qui doit être approvisionné
/// pour permettre les transactions. Le gas consommé revient au coordinator.
pub trait GasPoolStorage: Send + Sync {
    /// Récupère le gas pool d'un ledger.
    fn get_gas_pool(&self, ledger_id: &str) -> Result<Option<GasPool>>;

    /// Crée ou met à jour un gas pool.
    fn put_gas_pool(&self, pool: &GasPool) -> Result<()>;

    /// Liste tous les gas pools.
    fn list_gas_pools(&self) -> Result<Vec<GasPool>>;

    /// Atomic consume: read → deduct gas → write back.
    /// Returns the new balance on success.
    /// Fails if `balance - gas_per_tx < min_balance`.
    fn consume_gas(
        &self,
        ledger_id: &str,
        gas_per_tx: Decimal,
        min_balance: Decimal,
    ) -> Result<Decimal>;

    /// Atomic deposit: read → add amount → write back.
    /// Creates a new pool if none exists.
    /// Returns the new balance.
    fn deposit_gas(&self, ledger_id: &str, amount: Decimal) -> Result<Decimal>;

    /// Atomic withdraw: read → subtract amount → write back.
    /// Returns the new balance.
    fn withdraw_gas(&self, ledger_id: &str, amount: Decimal) -> Result<Decimal>;
}
