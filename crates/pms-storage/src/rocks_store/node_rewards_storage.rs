//! Implémentation de NodeRewardsStorage pour RocksStore.
//!
//! Stocke le pool de fees et les compteurs de blocs par nœud.

use crate::node_rewards::NodeRewardsStorage;
use crate::rocks_store::store::RocksStore;
use anyhow::Result;

/// Convertit des bytes en u64 (little endian).
fn bytes_to_u64(bytes: &[u8]) -> u64 {
    if bytes.len() >= 8 {
        u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])
    } else {
        0
    }
}

impl NodeRewardsStorage for RocksStore {
    /// Récupère le nombre de blocs minés par un nœud.
    fn get_node_block_count(&self, node_pk: &str) -> Result<u64> {
        let cf = self.cf("node_block_counts");
        match self.db.get_cf(&cf, node_pk.as_bytes())? {
            Some(bytes) => Ok(bytes_to_u64(&bytes)),
            None => Ok(0),
        }
    }

    /// Incrémente le compteur de blocs pour un nœud.
    fn increment_node_block_count(&self, node_pk: &str) -> Result<()> {
        let cf = self.cf("node_block_counts");
        // Single read + write (coordinator-only, no race condition)
        let current = match self.db.get_cf(&cf, node_pk.as_bytes())? {
            Some(bytes) => bytes_to_u64(&bytes),
            None => 0,
        };
        self.db
            .put_cf(&cf, node_pk.as_bytes(), (current + 1).to_le_bytes())?;
        Ok(())
    }

    /// Récupère le montant total dans le pool de fees.
    fn get_fee_pool(&self) -> Result<u64> {
        let cf = self.cf("node_fee_pool");
        match self.db.get_cf(&cf, b"pool")? {
            Some(bytes) => Ok(bytes_to_u64(&bytes)),
            None => Ok(0),
        }
    }

    /// Ajoute un montant au pool de fees.
    fn add_to_fee_pool(&self, amount: u64) -> Result<()> {
        let cf = self.cf("node_fee_pool");
        let current = match self.db.get_cf(&cf, b"pool")? {
            Some(bytes) => bytes_to_u64(&bytes),
            None => 0,
        };
        self.db.put_cf(&cf, b"pool", (current + amount).to_le_bytes())?;
        Ok(())
    }

    /// Récupère tous les mineurs et leurs compteurs de blocs.
    fn get_all_miners(&self) -> Result<Vec<(String, u64)>> {
        let cf = self.cf("node_block_counts");
        let mut miners = Vec::new();
        let iter = self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start);

        for result in iter {
            let (key, value) = result?;
            let node_pk = String::from_utf8_lossy(&key).to_string();
            let count = bytes_to_u64(&value);
            miners.push((node_pk, count));
        }

        Ok(miners)
    }

    /// Réinitialise le pool et tous les compteurs après distribution.
    fn reset_pool_and_counts(&self) -> Result<()> {
        // Reset pool
        let cf_pool = self.cf("node_fee_pool");
        self.db.put_cf(&cf_pool, b"pool", 0u64.to_le_bytes())?;

        // Reset all node counts
        let cf_counts = self.cf("node_block_counts");

        // Collect keys first to avoid borrowing issues
        let keys: Vec<_> = self
            .db
            .iterator_cf(&cf_counts, rocksdb::IteratorMode::Start)
            .filter_map(|r| r.ok().map(|(k, _)| k))
            .collect();

        let mut batch = rocksdb::WriteBatch::default();
        for key in keys {
            batch.delete_cf(&cf_counts, &key);
        }
        self.db.write(batch)?;

        Ok(())
    }

    /// Définit l'adresse de récompense pour un nœud.
    fn set_node_reward_address(&self, node_pk: &str, address: &str) -> Result<()> {
        let cf = self.cf("node_reward_addresses");
        self.db
            .put_cf(&cf, node_pk.as_bytes(), address.as_bytes())?;
        Ok(())
    }

    /// Récupère l'adresse de récompense d'un nœud (ou la clé publique par défaut).
    fn get_node_reward_address(&self, node_pk: &str) -> Result<String> {
        let cf = self.cf("node_reward_addresses");
        match self.db.get_cf(&cf, node_pk.as_bytes())? {
            Some(bytes) => Ok(String::from_utf8_lossy(&bytes).to_string()),
            None => Ok(node_pk.to_string()),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Fee Burn Tracking (cumulative total burned)
// ═══════════════════════════════════════════════════════════════════════

impl RocksStore {
    /// Atomically increment the cumulative total of burned fees.
    /// Stored as Decimal string in `node_fee_pool` CF under key `"total_burned"`.
    pub fn increment_total_burned(&self, amount: rust_decimal::Decimal) -> Result<()> {
        use rust_decimal::Decimal;
        if amount <= Decimal::ZERO {
            return Ok(());
        }
        let cf = self.cf("node_fee_pool");
        let current = match self.db.get_cf(&cf, b"total_burned")? {
            Some(bytes) => {
                let s = String::from_utf8_lossy(&bytes);
                Decimal::from_str_exact(&s).unwrap_or(Decimal::ZERO)
            }
            None => Decimal::ZERO,
        };
        let new_total = current + amount;
        self.db
            .put_cf(&cf, b"total_burned", new_total.to_string().as_bytes())?;
        Ok(())
    }

    /// Get the cumulative total of burned fees.
    pub fn get_total_burned(&self) -> Result<rust_decimal::Decimal> {
        use rust_decimal::Decimal;
        let cf = self.cf("node_fee_pool");
        match self.db.get_cf(&cf, b"total_burned")? {
            Some(bytes) => {
                let s = String::from_utf8_lossy(&bytes);
                Ok(Decimal::from_str_exact(&s).unwrap_or(Decimal::ZERO))
            }
            None => Ok(Decimal::ZERO),
        }
    }
}
