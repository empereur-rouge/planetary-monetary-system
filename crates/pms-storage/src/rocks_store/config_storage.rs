//! Implémentation de ConfigStorage pour RocksStore.
//!
//! Stocke la configuration runtime et son historique dans RocksDB.
//! Uses a 500ms TTL cache for `get_runtime_config()` to avoid
//! hitting RocksDB on every `persist_block()` call.

use crate::config_store::ConfigStorage;
use crate::rocks_store::store::RocksStore;
use anyhow::{Context, Result};
use pms_config::{ConfigHistoryEntry, RuntimeConfig};

impl ConfigStorage for RocksStore {
    /// Récupère la configuration runtime courante (cached, 500ms TTL).
    fn get_runtime_config(&self) -> Result<RuntimeConfig> {
        // Fast path: check cache (500ms TTL)
        if let Some((ts, ref config)) = *self.runtime_config_cache.lock() {
            if ts.elapsed() < std::time::Duration::from_millis(500) {
                return Ok(config.clone());
            }
        }

        // Cache miss: read from RocksDB
        let cf = self.cf("runtime_config");
        let config = match self.db.get_cf(&cf, b"current")? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .context("Failed to deserialize RuntimeConfig")?,
            None => RuntimeConfig::default(),
        };

        // Update cache
        *self.runtime_config_cache.lock() = Some((std::time::Instant::now(), config.clone()));

        Ok(config)
    }

    /// Persiste une nouvelle configuration runtime (write-through cache).
    fn set_runtime_config(&self, config: &RuntimeConfig) -> Result<()> {
        let cf = self.cf("runtime_config");
        let bytes = serde_json::to_vec(config)?;
        self.db.put_cf(&cf, b"current", &bytes)?;

        // Write-through: immediately update cache with new value
        *self.runtime_config_cache.lock() = Some((std::time::Instant::now(), config.clone()));

        Ok(())
    }

    /// Ajoute une entrée à l'historique des changements de config.
    fn append_config_history(&self, entry: &ConfigHistoryEntry) -> Result<()> {
        let cf = self.cf("config_history");

        // Clé: timestamp:block_id pour ordre chronologique
        let key = format!("{}:{}", entry.timestamp, entry.block_id);
        let bytes = serde_json::to_vec(entry)?;
        self.db.put_cf(&cf, key.as_bytes(), &bytes)?;
        Ok(())
    }

    /// Récupère l'historique complet des changements de config.
    fn get_config_history(&self) -> Result<Vec<ConfigHistoryEntry>> {
        let cf = self.cf("config_history");

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
