//! Trait pour le stockage de la configuration runtime.
//!
//! Permet de persister et récupérer la RuntimeConfig et son historique
//! de changements depuis RocksDB.

use anyhow::Result;
use pms_config::{ConfigHistoryEntry, ConfigUpdate, RuntimeConfig};

/// Trait abstrayant le stockage de la configuration runtime.
///
/// Implémenté par RocksStore pour la persistance.
pub trait ConfigStorage: Send + Sync {
    /// Récupère la configuration runtime courante.
    ///
    /// Retourne la config par défaut si aucune n'a été persistée.
    fn get_runtime_config(&self) -> Result<RuntimeConfig>;

    /// Persiste une nouvelle configuration runtime.
    fn set_runtime_config(&self, config: &RuntimeConfig) -> Result<()>;

    /// Applique une mise à jour et persiste le résultat.
    ///
    /// Retourne la nouvelle configuration après application.
    fn apply_config_update(
        &self,
        update: &ConfigUpdate,
        block_id: &str,
        timestamp: i64,
    ) -> Result<RuntimeConfig> {
        let current = self.get_runtime_config()?;
        let new_config = current.apply_update(update, block_id, timestamp);
        self.set_runtime_config(&new_config)?;

        // Ajouter à l'historique
        let entry = ConfigHistoryEntry {
            block_id: block_id.to_string(),
            timestamp,
            update: update.clone(),
            resulting_config: new_config.clone(),
        };
        self.append_config_history(&entry)?;

        Ok(new_config)
    }

    /// Ajoute une entrée à l'historique des changements de config.
    fn append_config_history(&self, entry: &ConfigHistoryEntry) -> Result<()>;

    /// Récupère l'historique complet des changements de config.
    fn get_config_history(&self) -> Result<Vec<ConfigHistoryEntry>>;
}
