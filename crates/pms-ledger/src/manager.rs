use crate::LedgerInstance;
use anyhow::{Context, Result, bail};
use dashmap::DashMap;
use pms_config::LedgerDef;
use pms_config::Settings;
use pms_storage::rocks_store::store::{PmsDb, RocksStore};
use rocksdb::Options;
use std::sync::Arc;

/// Gère la collection de ledgers actifs.
/// Thread-safe via DashMap. Partagé entre les handlers API via Arc.
pub struct LedgerManager {
    ledgers: DashMap<String, Arc<LedgerInstance>>,
    shared_db: Arc<PmsDb>,
    global_tip_limit: usize,
    global_max_dag_blocks: usize,
    global_max_spent_outpoints: usize,
}

impl LedgerManager {
    /// Crée un LedgerManager en ouvrant la DB partagée et bootstrappant tous les ledgers.
    pub async fn bootstrap(settings: &Settings) -> Result<Self> {
        let ledger_defs = settings.effective_ledgers();

        if ledger_defs.is_empty() {
            bail!("No ledgers defined (and no default could be inferred)");
        }

        // Collecter tous les prefixes pour ouvrir la DB avec toutes les CF
        let prefixes: Vec<String> = ledger_defs.iter().map(|d| d.prefix.clone()).collect();

        tracing::info!(
            count = ledger_defs.len(),
            prefixes = ?prefixes,
            "Opening shared RocksDB for multi-ledger"
        );

        let shared_db = RocksStore::open_db_multi_prefix(&settings.rocks.path, &prefixes)
            .await
            .context("opening shared RocksDB")?;

        let manager = Self {
            ledgers: DashMap::new(),
            shared_db: shared_db.clone(),
            global_tip_limit: settings.rocks.tip_limit,
            global_max_dag_blocks: settings.rocks.max_dag_blocks,
            global_max_spent_outpoints: settings.rocks.max_spent_outpoints,
        };

        // Bootstrap each ledger
        for def in ledger_defs {
            let instance = LedgerInstance::bootstrap(
                shared_db.clone(),
                def.clone(),
                settings.rocks.tip_limit,
                settings.rocks.max_dag_blocks,
                settings.rocks.max_spent_outpoints,
            )
            .await
            .with_context(|| format!("bootstrapping ledger '{}'", def.id))?;

            tracing::info!(ledger = %def.id, prefix = %def.prefix, "Ledger ready");
            manager.ledgers.insert(def.id.clone(), Arc::new(instance));
        }

        Ok(manager)
    }

    /// Retourne un ledger par son ID.
    pub fn get(&self, id: &str) -> Option<Arc<LedgerInstance>> {
        self.ledgers.get(id).map(|r| r.value().clone())
    }

    /// Retourne un ledger par son `network_id` (utilisé pour le routing P2P).
    pub fn get_by_network_id(&self, network_id: &str) -> Option<Arc<LedgerInstance>> {
        self.ledgers
            .iter()
            .find(|r| r.value().def.network_id == network_id)
            .map(|r| r.value().clone())
    }

    /// Retourne le ledger par défaut ("main").
    /// Fallback: retourne le premier ledger disponible.
    pub fn default_ledger(&self) -> Option<Arc<LedgerInstance>> {
        self.get("main").or_else(|| {
            self.ledgers.iter().next().map(|r| r.value().clone())
        })
    }

    /// Liste tous les IDs de ledgers actifs.
    pub fn list_ids(&self) -> Vec<String> {
        self.ledgers.iter().map(|r| r.key().clone()).collect()
    }

    /// Liste tous les ledgers actifs avec leurs métadonnées.
    pub fn list_all(&self) -> Vec<Arc<LedgerInstance>> {
        self.ledgers.iter().map(|r| r.value().clone()).collect()
    }

    /// Nombre de ledgers actifs.
    pub fn len(&self) -> usize {
        self.ledgers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ledgers.is_empty()
    }

    /// Retourne la référence vers la DB partagée.
    pub fn shared_db(&self) -> &Arc<PmsDb> {
        &self.shared_db
    }

    /// Crée dynamiquement un nouveau ledger.
    /// Crée les column families à chaud (grâce au mode MultiThreaded)
    /// puis bootstrap le genesis block, DAG et UTXOs.
    pub async fn add_ledger(&self, def: LedgerDef) -> Result<Arc<LedgerInstance>> {
        if self.ledgers.contains_key(&def.id) {
            bail!("Ledger '{}' already exists", def.id);
        }

        // Créer les CFs pour ce prefix si elles n'existent pas encore
        let test_cf = format!("{}:blocks", def.prefix);
        if self.shared_db.cf_handle(&test_cf).is_none() {
            tracing::info!(
                ledger = %def.id,
                prefix = %def.prefix,
                "Creating column families for new ledger"
            );
            for &cf_name in RocksStore::CF_NAMES {
                let full_name = format!("{}:{}", def.prefix, cf_name);
                if self.shared_db.cf_handle(&full_name).is_none() {
                    self.shared_db
                        .create_cf(&full_name, &Options::default())
                        .with_context(|| format!("creating CF '{}'", full_name))?;
                }
            }
        }

        let instance = LedgerInstance::bootstrap(
            self.shared_db.clone(),
            def.clone(),
            self.global_tip_limit,
            self.global_max_dag_blocks,
            self.global_max_spent_outpoints,
        )
        .await
        .with_context(|| format!("bootstrapping ledger '{}'", def.id))?;

        let instance = Arc::new(instance);
        self.ledgers.insert(def.id.clone(), instance.clone());
        tracing::info!(ledger = %def.id, "Ledger added dynamically");
        Ok(instance)
    }
}
