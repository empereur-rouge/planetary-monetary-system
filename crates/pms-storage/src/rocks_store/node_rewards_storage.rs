//! Implémentation de NodeRewardsStorage pour RocksStore.
//!
//! Stocke le pool de fees et les compteurs de blocs par nœud.

use crate::node_rewards::NodeRewardsStorage;
use crate::rocks_store::store::RocksStore;
use anyhow::{Context, Result};

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
        let cf = self
            .db
            .cf_handle(&format!("{}:node_block_counts", self.prefix))
            .context("CF node_block_counts not found")?;

        match self.db.get_cf(&cf, node_pk.as_bytes())? {
            Some(bytes) => Ok(bytes_to_u64(&bytes)),
            None => Ok(0),
        }
    }

    /// Incrémente le compteur de blocs pour un nœud.
    fn increment_node_block_count(&self, node_pk: &str) -> Result<()> {
        let current = self.get_node_block_count(node_pk)?;
        let new_count = current + 1;

        let cf = self
            .db
            .cf_handle(&format!("{}:node_block_counts", self.prefix))
            .context("CF node_block_counts not found")?;

        // NOTE: pas besoin de & car to_le_bytes() retourne un array qui impl AsRef<[u8]>
        self.db
            .put_cf(&cf, node_pk.as_bytes(), new_count.to_le_bytes())?;
        Ok(())
    }

    /// Récupère le montant total dans le pool de fees.
    fn get_fee_pool(&self) -> Result<u64> {
        let cf = self
            .db
            .cf_handle(&format!("{}:node_fee_pool", self.prefix))
            .context("CF node_fee_pool not found")?;

        match self.db.get_cf(&cf, b"pool")? {
            Some(bytes) => Ok(bytes_to_u64(&bytes)),
            None => Ok(0),
        }
    }

    /// Ajoute un montant au pool de fees.
    fn add_to_fee_pool(&self, amount: u64) -> Result<()> {
        let current = self.get_fee_pool()?;
        let new_amount = current + amount;

        let cf = self
            .db
            .cf_handle(&format!("{}:node_fee_pool", self.prefix))
            .context("CF node_fee_pool not found")?;

        // NOTE: pas besoin de & car to_le_bytes() retourne un array qui impl AsRef<[u8]>
        self.db.put_cf(&cf, b"pool", new_amount.to_le_bytes())?;
        Ok(())
    }

    /// Récupère tous les mineurs et leurs compteurs de blocs.
    fn get_all_miners(&self) -> Result<Vec<(String, u64)>> {
        let cf = self
            .db
            .cf_handle(&format!("{}:node_block_counts", self.prefix))
            .context("CF node_block_counts not found")?;

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
        let cf_pool = self
            .db
            .cf_handle(&format!("{}:node_fee_pool", self.prefix))
            .context("CF node_fee_pool not found")?;
        // NOTE: pas besoin de & car to_le_bytes() retourne un array qui impl AsRef<[u8]>
        self.db.put_cf(&cf_pool, b"pool", 0u64.to_le_bytes())?;

        // Reset all node counts
        let cf_counts = self
            .db
            .cf_handle(&format!("{}:node_block_counts", self.prefix))
            .context("CF node_block_counts not found")?;

        // Collect keys first to avoid borrowing issues
        let keys: Vec<_> = self
            .db
            .iterator_cf(&cf_counts, rocksdb::IteratorMode::Start)
            .filter_map(|r| r.ok().map(|(k, _)| k))
            .collect();

        for key in keys {
            self.db.delete_cf(&cf_counts, &key)?;
        }

        Ok(())
    }

    /// Définit l'adresse de récompense pour un nœud.
    fn set_node_reward_address(&self, node_pk: &str, address: &str) -> Result<()> {
        let cf = self
            .db
            .cf_handle(&format!("{}:node_reward_addresses", self.prefix))
            .context("CF node_reward_addresses not found")?;

        self.db.put_cf(&cf, node_pk.as_bytes(), address.as_bytes())?;
        Ok(())
    }

    /// Récupère l'adresse de récompense d'un nœud (ou la clé publique par défaut).
    fn get_node_reward_address(&self, node_pk: &str) -> Result<String> {
        let cf = self
            .db
            .cf_handle(&format!("{}:node_reward_addresses", self.prefix))
            .context("CF node_reward_addresses not found")?;

        match self.db.get_cf(&cf, node_pk.as_bytes())? {
            Some(bytes) => Ok(String::from_utf8_lossy(&bytes).to_string()),
            None => Ok(node_pk.to_string()), // Par défaut, utilise la clé publique
        }
    }
}
