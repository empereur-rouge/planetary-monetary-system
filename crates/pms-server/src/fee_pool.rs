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
    /// Total des fees accumulées (en PMS)
    pub total_fees: Decimal,
    /// Contributions par nœud: node_pk -> nombre de blocs créés
    pub node_contributions: HashMap<String, u64>,
    /// Compteur total de transactions traitées (pour stats)
    pub tx_count: u64,
}

impl FeePool {
    pub fn new() -> Self {
        Self {
            total_fees: Decimal::ZERO,
            node_contributions: HashMap::new(),
            tx_count: 0,
        }
    }

    /// Ajoute une fee au pool et incrémente le compteur du nœud
    pub fn add_fee(&mut self, fee: Decimal, node_pk: &str) {
        self.total_fees += fee;
        self.tx_count += 1;
        *self
            .node_contributions
            .entry(node_pk.to_string())
            .or_insert(0) += 1;
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
                let amount = self.total_fees * share;
                (pk.clone(), share, amount)
            })
            .collect()
    }

    /// Remet le pool à zéro après distribution
    pub fn reset(&mut self) {
        self.total_fees = Decimal::ZERO;
        self.node_contributions.clear();
        self.tx_count = 0;
    }

    /// Retourne true si le pool a des fees à distribuer
    pub fn has_fees(&self) -> bool {
        self.total_fees > Decimal::ZERO
    }
}

/// Type thread-safe pour le fee pool
pub type SharedFeePool = Arc<RwLock<FeePool>>;

/// Crée un nouveau fee pool partagé
pub fn create_fee_pool() -> SharedFeePool {
    Arc::new(RwLock::new(FeePool::new()))
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
}
