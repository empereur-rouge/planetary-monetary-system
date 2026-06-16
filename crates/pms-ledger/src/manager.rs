use crate::{LedgerInstance, LedgerLockSource, LedgerStoreResolver};
use anyhow::{Context, Result, bail};
use dashmap::DashMap;
use pms_config::LedgerDef;
use pms_config::Settings;
use pms_interface::BridgeLockResolver;
use pms_storage::rocks_store::store::{PmsDb, RocksMemoryConfig, RocksStore};
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
    global_max_utxos: usize,
    /// Shared `ledger_id -> (dag, store)` map backing the cross-ledger
    /// [`LedgerStoreResolver`] used for `BridgeMint` reconciliation (audit rang 3,
    /// B3). Holds DAGs + stores only (no adapters) → no `Arc` cycle. Updated in
    /// lockstep with `ledgers` so dynamically-added ledgers are resolvable.
    bridge_sources: Arc<DashMap<String, LedgerLockSource>>,
    /// The single cross-ledger reconciliation resolver, wired into every adapter.
    /// Built once over `bridge_sources` (which it shares), so a ledger added later
    /// via `add_ledger` is resolvable without rebuilding it (audit rang 3, B3).
    bridge_resolver: Arc<dyn BridgeLockResolver>,
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

        let mem_config = RocksMemoryConfig {
            write_buffer_size_mb: settings.rocks.write_buffer_size_mb,
            max_write_buffer_number: settings.rocks.max_write_buffer_number,
            block_cache_size_mb: settings.rocks.block_cache_size_mb,
            db_write_buffer_size_mb: settings.rocks.db_write_buffer_size_mb,
            max_open_files: settings.rocks.max_open_files,
        };

        let shared_db =
            RocksStore::open_db_multi_prefix(&settings.rocks.path, &prefixes, &mem_config)
                .await
                .context("opening shared RocksDB")?;

        // Build the shared source map + the single reconciliation resolver BEFORE
        // the manager so the resolver is a manager field reused by `add_ledger`
        // (audit rang 3, B3). The resolver shares `bridge_sources`, so ledgers
        // added later are resolvable without rebuilding it.
        let bridge_sources: Arc<DashMap<String, LedgerLockSource>> = Arc::new(DashMap::new());
        let bridge_resolver: Arc<dyn BridgeLockResolver> =
            Arc::new(LedgerStoreResolver::new(bridge_sources.clone()));

        let manager = Self {
            ledgers: DashMap::new(),
            shared_db: shared_db.clone(),
            global_tip_limit: settings.rocks.tip_limit,
            global_max_dag_blocks: settings.rocks.max_dag_blocks,
            global_max_spent_outpoints: settings.rocks.max_spent_outpoints,
            global_max_utxos: settings.rocks.max_utxos,
            bridge_sources,
            bridge_resolver,
        };

        // Bootstrap each ledger
        for def in ledger_defs {
            let instance = LedgerInstance::bootstrap(
                shared_db.clone(),
                def.clone(),
                settings.rocks.tip_limit,
                settings.rocks.max_dag_blocks,
                settings.rocks.max_spent_outpoints,
                settings.rocks.max_utxos,
            )
            .await
            .with_context(|| format!("bootstrapping ledger '{}'", def.id))?;

            tracing::info!(ledger = %def.id, prefix = %def.prefix, "Ledger ready");
            let instance = Arc::new(instance);
            manager.register_bridge_source(&def.id, &instance);
            manager.ledgers.insert(def.id.clone(), instance);
        }

        // Wire the cross-ledger BridgeMint reconciliation resolver + each adapter's
        // own ledger id into every adapter (audit rang 3, B3). Done AFTER all
        // ledgers are registered so each adapter can resolve a source `BridgeLock`
        // on ANY ledger.
        for entry in manager.ledgers.iter() {
            entry
                .value()
                .adapter
                .set_bridge_resolver(manager.bridge_resolver.clone(), entry.key().clone());
        }

        Ok(manager)
    }

    /// Register a ledger's `(dag, store)` in the shared cross-ledger source map so
    /// its `BridgeLock`s become resolvable for `BridgeMint` reconciliation
    /// (audit rang 3, B3). Holds DAG + store only — never the adapter — so no
    /// `Arc` cycle is created.
    fn register_bridge_source(&self, id: &str, instance: &LedgerInstance) {
        self.bridge_sources.insert(
            id.to_string(),
            LedgerLockSource {
                dag: instance.dag.clone(),
                store: instance.store.clone(),
            },
        );
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
        self.get("main")
            .or_else(|| self.ledgers.iter().next().map(|r| r.value().clone()))
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

        // Ensure ALL CFs exist for this prefix (handles both new ledgers
        // and existing ledgers that predate newly-added CFs like "contracts")
        {
            let mut created = 0usize;
            for &cf_name in RocksStore::CF_NAMES {
                let full_name = format!("{}:{}", def.prefix, cf_name);
                if self.shared_db.cf_handle(&full_name).is_none() {
                    self.shared_db
                        .create_cf(&full_name, &Options::default())
                        .with_context(|| format!("creating CF '{}'", full_name))?;
                    created += 1;
                }
            }
            if created > 0 {
                tracing::info!(
                    ledger = %def.id,
                    prefix = %def.prefix,
                    created,
                    "Created column families for ledger"
                );
            }
        }

        let instance = LedgerInstance::bootstrap(
            self.shared_db.clone(),
            def.clone(),
            self.global_tip_limit,
            self.global_max_dag_blocks,
            self.global_max_spent_outpoints,
            self.global_max_utxos,
        )
        .await
        .with_context(|| format!("bootstrapping ledger '{}'", def.id))?;

        let instance = Arc::new(instance);
        // Register the source + wire the (shared) reconciliation resolver on the
        // new adapter (audit rang 3, B3). The resolver shares `bridge_sources`, so
        // the new ledger is also resolvable from every previously-wired adapter.
        self.register_bridge_source(&def.id, &instance);
        instance
            .adapter
            .set_bridge_resolver(self.bridge_resolver.clone(), def.id.clone());
        self.ledgers.insert(def.id.clone(), instance.clone());
        tracing::info!(ledger = %def.id, "Ledger added dynamically");
        Ok(instance)
    }

    /// Met à jour la LedgerDef d'un ledger existant en mémoire.
    ///
    /// Utilisé pour le transfert d'ownership et autres modifications de métadonnées.
    /// Remplace l'instance Arc dans le DashMap en conservant DAG, UTXOs et store.
    pub fn update_def(&self, id: &str, new_def: LedgerDef) {
        if let Some(mut entry) = self.ledgers.get_mut(id) {
            let old = entry.value().clone();
            let updated = Arc::new(LedgerInstance {
                id: old.id.clone(),
                dag: old.dag.clone(),
                utxos: old.utxos.clone(),
                store: old.store.clone(),
                adapter: old.adapter.clone(),
                def: new_def,
            });
            *entry = updated;
            tracing::debug!(ledger = %id, "LedgerDef updated in-memory");
        }
    }

    /// Charge les définitions de ledgers persistées en RocksDB.
    ///
    /// Appelé après `bootstrap()` pour restaurer :
    /// 1. Les changements d'ownership (ledger existe déjà en RAM, mais owner_pubkey a changé)
    /// 2. Les ledgers créés dynamiquement via l'API (pas dans config.toml)
    pub async fn load_persisted_ledgers(
        &self,
        store: &dyn pms_storage::LedgerDefStorage,
    ) -> Result<()> {
        let persisted = store.list_ledger_defs()?;
        if persisted.is_empty() {
            return Ok(());
        }

        tracing::info!(count = persisted.len(), "Loading persisted ledger definitions from RocksDB");

        for def in persisted {
            if let Some(existing) = self.ledgers.get(&def.id) {
                // Ledger exists in RAM — check if ownership changed
                if existing.def.owner_pubkey != def.owner_pubkey {
                    tracing::info!(
                        ledger = %def.id,
                        old_owner = ?existing.def.owner_pubkey,
                        new_owner = ?def.owner_pubkey,
                        "Restoring ownership from RocksDB"
                    );
                    drop(existing); // release DashMap ref before mutating
                    let id = def.id.clone();
                    self.update_def(&id, def);
                }
            } else {
                // Ledger doesn't exist in RAM — dynamically created, restore it
                tracing::info!(ledger = %def.id, "Restoring dynamic ledger from RocksDB");
                if let Err(e) = self.add_ledger(def).await {
                    tracing::warn!("Failed to restore persisted ledger: {e}");
                }
            }
        }

        Ok(())
    }
}
