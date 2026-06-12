// ============================================================================
// Fee Pool - Accumulates transaction fees for periodic distribution
// ============================================================================
//
// Fees sont accumulées ici au lieu d'être distribuées immédiatement.
// Le Coordinator distribue le pool via Milestone avec distribute_node_rewards=true

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Pool de fees accumulées en attente de distribution
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FeePool {
    /// Total des fees accumulées (en PMS) - pour distribution aux nœuds
    pub total_fees: Decimal,
    /// Contributions par nœud: node_pk -> nombre de blocs créés
    pub node_contributions: HashMap<String, u64>,
    /// Compteur total de transactions traitées (pour stats)
    pub tx_count: u64,
    /// Refunds de burn en attente: (wallet_address, asset_id) -> montant total
    /// asset_id = None pour PMS natif, Some("edenite") pour custom token
    /// Ces refunds sont distribués directement aux wallets utilisateurs
    pub burn_refunds: HashMap<(String, Option<String>), Decimal>,
}

impl FeePool {
    pub fn new() -> Self {
        Self {
            total_fees: Decimal::ZERO,
            node_contributions: HashMap::new(),
            tx_count: 0,
            burn_refunds: HashMap::new(),
        }
    }

    /// Ajoute une fee au pool et incrémente le compteur du nœud
    /// Utilisé pour les fees de transactions (distribuées aux nœuds)
    pub fn add_fee(&mut self, fee: Decimal, node_pk: &str) {
        self.total_fees += fee;
        self.tx_count += 1;
        *self
            .node_contributions
            .entry(node_pk.to_string())
            .or_insert(0) += 1;
    }

    /// Ajoute un refund de burn pour un wallet utilisateur
    /// Ces refunds sont séparés des fees de nœuds et vont directement aux wallets
    /// `asset_id`: None = PMS natif, Some("edenite") = custom token
    pub fn add_burn_refund(
        &mut self,
        wallet_address: &str,
        amount: Decimal,
        asset_id: Option<String>,
    ) {
        *self
            .burn_refunds
            .entry((wallet_address.to_string(), asset_id))
            .or_insert(Decimal::ZERO) += amount;
    }

    /// Retourne les refunds de burn en attente: Vec<(wallet_address, asset_id, amount)>
    pub fn get_burn_refunds(&self) -> Vec<(String, Option<String>, Decimal)> {
        self.burn_refunds
            .iter()
            .filter(|(_, amount)| **amount > Decimal::ZERO)
            .map(|((addr, asset_id), amount)| (addr.clone(), asset_id.clone(), *amount))
            .collect()
    }

    /// Total des refunds de burn PMS natif en attente (pour stats/logs)
    pub fn total_burn_refunds(&self) -> Decimal {
        self.burn_refunds
            .iter()
            .filter(|((_, asset_id), _)| asset_id.is_none())
            .map(|(_, amount)| *amount)
            .sum()
    }

    /// Calcule les parts proportionnelles de chaque nœud
    /// Retourne: Vec<(node_pk, share_percentage, fee_amount)>
    pub fn calculate_shares(&self) -> Vec<(String, Decimal, Decimal)> {
        let total_blocks: u64 = self.node_contributions.values().sum();
        if total_blocks == 0 {
            return vec![];
        }

        self.node_contributions
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(pk, count)| {
                let share = Decimal::from(*count) / Decimal::from(total_blocks);
                let amount = (self.total_fees * share).round_dp(8);
                (pk.clone(), share, amount)
            })
            .collect()
    }

    /// Remet le pool à zéro après distribution
    pub fn reset(&mut self) {
        self.total_fees = Decimal::ZERO;
        self.node_contributions.clear();
        self.tx_count = 0;
        self.burn_refunds.clear();
    }

    /// Merge another pool's data into this one (for error recovery).
    ///
    /// Used when fee distribution fails after an atomic swap: the swapped-out
    /// snapshot must be restored to prevent permanent fee loss.
    pub fn merge_from(&mut self, other: &FeePool) {
        self.total_fees += other.total_fees;
        self.tx_count += other.tx_count;
        for (pk, count) in &other.node_contributions {
            *self.node_contributions.entry(pk.clone()).or_insert(0) += count;
        }
        for ((addr, asset), amount) in &other.burn_refunds {
            *self
                .burn_refunds
                .entry((addr.clone(), asset.clone()))
                .or_insert(Decimal::ZERO) += amount;
        }
    }

    /// Retourne true si le pool a des fees ou refunds à distribuer
    pub fn has_fees(&self) -> bool {
        self.total_fees > Decimal::ZERO || !self.burn_refunds.is_empty()
    }
}

/// Type thread-safe pour le fee pool
pub type SharedFeePool = Arc<RwLock<FeePool>>;

/// Crée un nouveau fee pool partagé
pub fn create_fee_pool() -> SharedFeePool {
    Arc::new(RwLock::new(FeePool::new()))
}

// ════════════════════════════════════════════════════════════════════════════
// Per-Ledger Fee Pool Registry
// ════════════════════════════════════════════════════════════════════════════

/// Registry of per-ledger fee pools.
/// Thread-safe via DashMap for lock-free concurrent access.
/// Each ledger (main, eden, etc.) gets its own isolated fee pool.
pub struct FeePoolRegistry {
    pools: dashmap::DashMap<String, SharedFeePool>,
}

impl FeePoolRegistry {
    pub fn new() -> Self {
        Self {
            pools: dashmap::DashMap::new(),
        }
    }

    /// Get or lazily create a fee pool for the given ledger.
    /// Idempotent: returns the same pool on repeated calls for the same ledger.
    pub fn get_or_create(&self, ledger_id: &str) -> SharedFeePool {
        self.pools
            .entry(ledger_id.to_string())
            .or_insert_with(create_fee_pool)
            .clone()
    }

    /// Snapshot of all (ledger_id, pool) pairs.
    pub fn all_pools(&self) -> Vec<(String, SharedFeePool)> {
        self.pools
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_pool_shares() {
        let mut pool = FeePool::new();

        // Node1: 3 blocks, Node2: 1 block
        pool.add_fee(Decimal::from(10), "node1");
        pool.add_fee(Decimal::from(10), "node1");
        pool.add_fee(Decimal::from(10), "node1");
        pool.add_fee(Decimal::from(10), "node2");

        assert_eq!(pool.total_fees, Decimal::from(40));

        let shares = pool.calculate_shares();
        assert_eq!(shares.len(), 2);

        // Node1: 75% (3/4), Node2: 25% (1/4)
        let node1 = shares.iter().find(|(pk, _, _)| pk == "node1").unwrap();
        let node2 = shares.iter().find(|(pk, _, _)| pk == "node2").unwrap();

        assert_eq!(node1.2, Decimal::from(30)); // 75% of 40
        assert_eq!(node2.2, Decimal::from(10)); // 25% of 40
    }

    #[test]
    fn test_burn_refund_pms_native() {
        let mut pool = FeePool::new();
        pool.add_burn_refund("wallet_a", Decimal::from(5), None);
        pool.add_burn_refund("wallet_a", Decimal::from(3), None);

        let refunds = pool.get_burn_refunds();
        assert_eq!(refunds.len(), 1);
        let (addr, asset_id, amount) = &refunds[0];
        assert_eq!(addr, "wallet_a");
        assert_eq!(*asset_id, None);
        assert_eq!(*amount, Decimal::from(8));
        assert_eq!(pool.total_burn_refunds(), Decimal::from(8));
        println!("PMS native refund: addr={}, asset={:?}, amount={}", addr, asset_id, amount);
    }

    #[test]
    fn test_burn_refund_custom_asset() {
        let mut pool = FeePool::new();
        pool.add_burn_refund("wallet_b", Decimal::from(10), Some("edenite".into()));
        pool.add_burn_refund("wallet_b", Decimal::from(7), Some("edenite".into()));

        let refunds = pool.get_burn_refunds();
        assert_eq!(refunds.len(), 1);
        let (addr, asset_id, amount) = &refunds[0];
        assert_eq!(addr, "wallet_b");
        assert_eq!(*asset_id, Some("edenite".into()));
        assert_eq!(*amount, Decimal::from(17));
        // Custom assets should NOT count towards total_burn_refunds (PMS stats)
        assert_eq!(pool.total_burn_refunds(), Decimal::ZERO);
        println!("Custom asset refund: addr={}, asset={:?}, amount={}", addr, asset_id, amount);
    }

    #[test]
    fn test_burn_refund_same_address_different_assets() {
        let mut pool = FeePool::new();
        pool.add_burn_refund("wallet_c", Decimal::from(5), None);
        pool.add_burn_refund("wallet_c", Decimal::from(100), Some("edenite".into()));

        let refunds = pool.get_burn_refunds();
        assert_eq!(refunds.len(), 2, "Same address, different assets = separate entries");

        let pms_entry = refunds.iter().find(|(_, a, _)| a.is_none()).unwrap();
        let edn_entry = refunds
            .iter()
            .find(|(_, a, _)| a.as_deref() == Some("edenite"))
            .unwrap();

        assert_eq!(pms_entry.0, "wallet_c");
        assert_eq!(pms_entry.2, Decimal::from(5));
        assert_eq!(edn_entry.0, "wallet_c");
        assert_eq!(edn_entry.2, Decimal::from(100));

        // Only PMS native counts
        assert_eq!(pool.total_burn_refunds(), Decimal::from(5));

        println!(
            "Multi-asset: PMS={}, EDN={}, total_pms={}",
            pms_entry.2, edn_entry.2, pool.total_burn_refunds()
        );
    }

    #[test]
    fn test_burn_refund_has_fees_with_custom_asset_only() {
        let mut pool = FeePool::new();
        assert!(!pool.has_fees());

        pool.add_burn_refund("wallet_d", Decimal::from(50), Some("edenite".into()));
        assert!(pool.has_fees(), "Pool should have fees even with only custom-asset refunds");
        assert_eq!(pool.total_burn_refunds(), Decimal::ZERO, "PMS total should be 0");
        println!("has_fees={} with custom asset only, total_pms={}", pool.has_fees(), pool.total_burn_refunds());
    }

    #[test]
    fn test_fee_pool_precision() {
        let mut pool = FeePool::new();

        // Total fees = 100 PMS (exact)
        pool.add_fee(Decimal::from(100), "node1");

        // 3 blocks total: Node1 (2 blocks), Node2 (1 block)
        // Node1 share = 2/3 * 100 = 66.666666666... -> 66.66666667
        // Node2 share = 1/3 * 100 = 33.333333333... -> 33.33333333
        pool.node_contributions.insert("node1".to_string(), 2);
        pool.node_contributions.insert("node2".to_string(), 1);

        let shares = pool.calculate_shares();
        let node1 = shares.iter().find(|(pk, _, _)| pk == "node1").unwrap();
        let node2 = shares.iter().find(|(pk, _, _)| pk == "node2").unwrap();

        assert_eq!(node1.2.to_string(), "66.66666667");
        assert_eq!(node2.2.to_string(), "33.33333333");
    }

    // ════════════════════════════════════════════════════════════════════════
    // FeePoolRegistry tests
    // ════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_registry_get_or_create_idempotent() {
        let registry = FeePoolRegistry::new();

        let pool_a1 = registry.get_or_create("eden");
        let pool_a2 = registry.get_or_create("eden");

        // Same Arc — must be the exact same pool
        assert!(Arc::ptr_eq(&pool_a1, &pool_a2), "get_or_create must return the same Arc");

        // Mutate via first handle, visible through second
        pool_a1.write().await.add_fee(Decimal::from(42), "node1");
        let total = pool_a2.read().await.total_fees;
        assert_eq!(total, Decimal::from(42));

        println!(
            "Idempotent: pool_a1 == pool_a2 = {}, total_fees = {}",
            Arc::ptr_eq(&pool_a1, &pool_a2),
            total
        );
    }

    #[tokio::test]
    async fn test_registry_ledger_isolation() {
        let registry = FeePoolRegistry::new();

        let main_pool = registry.get_or_create("main");
        let eden_pool = registry.get_or_create("eden");

        // They must be DIFFERENT pools
        assert!(!Arc::ptr_eq(&main_pool, &eden_pool), "Different ledgers must have separate pools");

        // Add fees to main only
        main_pool.write().await.add_fee(Decimal::from(100), "node1");
        // Add burn refund to eden only
        eden_pool
            .write()
            .await
            .add_burn_refund("wallet_a", Decimal::from(50), Some("edenite".into()));

        let main_total = main_pool.read().await.total_fees;
        let eden_total = eden_pool.read().await.total_fees;
        let eden_refunds = eden_pool.read().await.get_burn_refunds();

        assert_eq!(main_total, Decimal::from(100));
        assert_eq!(eden_total, Decimal::ZERO, "Eden pool must not have main's fees");
        assert_eq!(eden_refunds.len(), 1);
        assert_eq!(eden_refunds[0].2, Decimal::from(50));

        println!(
            "Isolation: main_fees={}, eden_fees={}, eden_refunds={}",
            main_total, eden_total, eden_refunds[0].2
        );
    }

    // ════════════════════════════════════════════════════════════════════════
    // No-loss-on-failure (merge_from) — audit gap E2 (v0.9.3)
    //
    // `perform_fee_distribution` (distribute.rs) fait un SWAP atomique
    // `snapshot = mem::replace(&mut *pool, FeePool::new())` puis, si le
    // persist_block du Reward échoue (Rejected / Err / non-Inserted), restaure
    // via `pool.merge_from(&snapshot)`. Sans ça, les fees swappées seraient
    // perdues définitivement. Ces tests reproduisent cette séquence exacte.
    // ════════════════════════════════════════════════════════════════════════

    #[test]
    fn merge_from_restores_every_field() {
        let mut snapshot = FeePool::new();
        snapshot.add_fee(Decimal::from(10), "node1");
        snapshot.add_fee(Decimal::from(10), "node1");
        snapshot.add_fee(Decimal::from(5), "node2");
        snapshot.add_burn_refund("wallet_x", Decimal::from(7), None);
        snapshot.add_burn_refund("wallet_y", Decimal::from(3), Some("edenite".into()));

        let mut target = FeePool::new();
        target.merge_from(&snapshot);

        println!(
            "merged: total_fees={}, tx_count={}, nodes={:?}, refunds={}",
            target.total_fees,
            target.tx_count,
            target.node_contributions,
            target.get_burn_refunds().len()
        );
        assert_eq!(target.total_fees, Decimal::from(25));
        assert_eq!(target.tx_count, 3);
        assert_eq!(target.node_contributions.get("node1"), Some(&2));
        assert_eq!(target.node_contributions.get("node2"), Some(&1));
        assert_eq!(target.get_burn_refunds().len(), 2);
        assert!(target.has_fees());
    }

    #[test]
    fn swap_then_failed_persist_restore_is_lossless() {
        // Pool accumulé avant distribution.
        let mut pool = FeePool::new();
        pool.add_fee(Decimal::from(10), "node1");
        pool.add_fee(Decimal::from(10), "node1");
        pool.add_fee(Decimal::from(5), "node2");
        pool.add_burn_refund("wallet_x", Decimal::from(7), None);
        let pre_total = pool.total_fees;
        let pre_tx = pool.tx_count;
        let pre_shares = pool.calculate_shares();

        // 1) SWAP atomique (cf. distribute.rs:107).
        let snapshot = std::mem::replace(&mut pool, FeePool::new());
        assert!(!pool.has_fees(), "pool must be empty right after the swap");
        assert_eq!(snapshot.total_fees, pre_total);

        // 2) Le persist_block du Reward ÉCHOUE (Rejected / Err) → restauration.
        pool.merge_from(&snapshot);

        // 3) Pool intégralement restauré : zéro fee perdue.
        println!(
            "after restore: total_fees={} (was {pre_total}), tx_count={} (was {pre_tx})",
            pool.total_fees, pool.tx_count
        );
        assert_eq!(pool.total_fees, pre_total, "restored total must equal pre-swap");
        assert_eq!(pool.tx_count, pre_tx);
        assert_eq!(pool.node_contributions, snapshot.node_contributions);
        assert_eq!(pool.get_burn_refunds().len(), 1);
        assert!(pool.has_fees());
        // Les parts recalculées après restore sont identiques à avant le swap.
        let post_shares = pool.calculate_shares();
        assert_eq!(pre_shares.len(), post_shares.len());
    }

    #[test]
    fn merge_from_preserves_fees_accrued_during_distribution_window() {
        // Scénario réaliste : entre le SWAP et l'échec du persist, de nouvelles
        // fees arrivent sur le pool fraîchement vidé. La restauration doit ADD
        // le snapshot par-dessus, sans écraser les fees concurrentes.
        let mut pool = FeePool::new();
        pool.add_fee(Decimal::from(40), "node1"); // 4 blocs node1
        pool.add_fee(Decimal::from(40), "node1");
        pool.add_fee(Decimal::from(40), "node1");
        pool.add_fee(Decimal::from(40), "node1");
        let snapshot = std::mem::replace(&mut pool, FeePool::new());

        // Fee concurrente qui tombe pendant la fenêtre de distribution.
        pool.add_fee(Decimal::from(7), "node2");

        // Persist échoue → restore.
        pool.merge_from(&snapshot);

        println!(
            "concurrent-window: total_fees={}, tx_count={}, node1={:?}, node2={:?}",
            pool.total_fees,
            pool.tx_count,
            pool.node_contributions.get("node1"),
            pool.node_contributions.get("node2")
        );
        // 160 (snapshot) + 7 (concurrent) = 167, AUCUNE perte.
        assert_eq!(pool.total_fees, Decimal::from(167));
        assert_eq!(pool.tx_count, 5); // 4 + 1
        assert_eq!(pool.node_contributions.get("node1"), Some(&4));
        assert_eq!(pool.node_contributions.get("node2"), Some(&1));
    }

    #[tokio::test]
    async fn test_registry_all_pools() {
        let registry = FeePoolRegistry::new();

        // Create 3 ledgers
        registry.get_or_create("main");
        registry.get_or_create("eden");
        registry.get_or_create("test");

        let all = registry.all_pools();
        assert_eq!(all.len(), 3);

        let ids: Vec<&str> = all.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"main"));
        assert!(ids.contains(&"eden"));
        assert!(ids.contains(&"test"));

        println!("all_pools: {:?}", ids);
    }
}
