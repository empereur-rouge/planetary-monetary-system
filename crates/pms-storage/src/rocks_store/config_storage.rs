//! Implémentation de ConfigStorage pour RocksStore.
//!
//! Stocke la configuration runtime et son historique dans RocksDB.

use crate::config_store::ConfigStorage;
use crate::rocks_store::store::RocksStore;
use anyhow::{Context, Result};
use pms_config::{ConfigHistoryEntry, RuntimeConfig};

impl ConfigStorage for RocksStore {
    /// Récupère la configuration runtime courante.
    ///
    /// Retourne la config par défaut si aucune n'a été persistée.
    fn get_runtime_config(&self) -> Result<RuntimeConfig> {
        let cf = self
            .db
            .cf_handle(&format!("{}:runtime_config", self.prefix))
            .context("CF runtime_config not found")?;

        match self.db.get_cf(&cf, b"current")? {
            Some(bytes) => {
                let config: RuntimeConfig = serde_json::from_slice(&bytes)
                    .context("Failed to deserialize RuntimeConfig")?;
                Ok(config)
            }
            None => Ok(RuntimeConfig::default()),
        }
    }

    /// Persiste une nouvelle configuration runtime.
    fn set_runtime_config(&self, config: &RuntimeConfig) -> Result<()> {
        let cf = self
            .db
            .cf_handle(&format!("{}:runtime_config", self.prefix))
            .context("CF runtime_config not found")?;

        let bytes = serde_json::to_vec(config)?;
        self.db.put_cf(&cf, b"current", &bytes)?;
        Ok(())
    }

    /// Ajoute une entrée à l'historique des changements de config.
    fn append_config_history(&self, entry: &ConfigHistoryEntry) -> Result<()> {
        let cf = self
            .db
            .cf_handle(&format!("{}:config_history", self.prefix))
            .context("CF config_history not found")?;

        // Clé: timestamp:block_id pour ordre chronologique
        let key = format!("{}:{}", entry.timestamp, entry.block_id);
        let bytes = serde_json::to_vec(entry)?;
        self.db.put_cf(&cf, key.as_bytes(), &bytes)?;
        Ok(())
    }

    /// Récupère l'historique complet des changements de config.
    fn get_config_history(&self) -> Result<Vec<ConfigHistoryEntry>> {
        let cf = self
            .db
            .cf_handle(&format!("{}:config_history", self.prefix))
            .context("CF config_history not found")?;

        let mut entries = Vec::new();
        let iter = self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start);

        for result in iter {
            let (_, value) = result?;
            let entry: ConfigHistoryEntry = serde_json::from_slice(&value)
                .context("Failed to deserialize ConfigHistoryEntry")?;
            entries.push(entry);
        }

        Ok(entries)
    }
}
