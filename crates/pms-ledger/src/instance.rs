use anyhow::{Context, Result};
use pms_config::LedgerDef;
use pms_core::CoreAdapter;
use pms_core::concurrent_dag::ConcurrentDag;
use pms_core::utxo::{ShardedUtxoSet, UtxoFetcher};
use pms_interface::NetDagAdapter;
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::PmsDb;
use pms_storage::rocks_store::store::RocksStore;
use pms_types_block::Block;
use pms_utils::compute_block_id;
use pms_wire::WireMeta;
use std::sync::Arc;

/// Un ledger complet et autonome.
/// Chaque instance encapsule son propre DAG, UTXO set, store et adapter.
pub struct LedgerInstance {
    /// Identifiant unique du ledger (ex: "main", "nft", "client-acme")
    pub id: String,
    /// DAG concurrent lock-free en RAM
    pub dag: Arc<ConcurrentDag>,
    /// Cache UTXO partitionné (256 shards)
    pub utxos: Arc<ShardedUtxoSet>,
    /// Store RocksDB (avec son prefix)
    pub store: Arc<RocksStore>,
    /// Adapter core (implémente NetDagAdapter)
    pub adapter: Arc<dyn NetDagAdapter>,
    /// Définition de ce ledger (network_id, protocol_version, etc.)
    pub def: LedgerDef,
}

impl LedgerInstance {
    /// Bootstrap un ledger à partir d'une DB partagée et d'une définition.
    /// Crée le genesis block si nécessaire, charge le DAG en RAM, bootstrap les UTXOs.
    pub async fn bootstrap(
        shared_db: Arc<PmsDb>,
        def: LedgerDef,
        global_tip_limit: usize,
        max_dag_blocks: usize,
        max_spent_outpoints: usize,
        max_utxos: usize,
    ) -> Result<Self> {
        let tip_limit = def.tip_limit.unwrap_or(global_tip_limit);

        // Crée un RocksStore pointant vers la DB partagée avec ce prefix
        let store = Arc::new(RocksStore::from_shared_db(
            shared_db,
            &def.prefix,
            tip_limit,
            None,
        ));

        // Ensure all column families exist (handles schema upgrades adding new CFs)
        store
            .ensure_column_families()
            .with_context(|| format!("ensure_column_families for ledger '{}'", def.id))?;

        if let Err(e) = store.ensure_schema().await {
            tracing::warn!(ledger = %def.id, "ensure_schema: {e}");
        }

        // Load frozen addresses into in-memory cache (O(1) lookups on hot path)
        if let Err(e) = store.load_frozen_cache() {
            tracing::warn!(ledger = %def.id, "load_frozen_cache: {e}");
        }

        // Genesis block si DB vide pour ce prefix
        let ids = store.all_block_ids().await.context("listing block IDs")?;
        let is_fresh_db = ids.is_empty();

        if is_fresh_db {
            let genesis = Block::genesis(compute_block_id);
            let meta = WireMeta {
                network_id: def.network_id.clone(),
                protocol_version: def.protocol_version,
            };
            store
                .persist_genesis(&genesis, &meta)
                .await
                .context("persisting genesis")?;
            tracing::info!(ledger = %def.id, "Genesis block created");
        }

        // ── DAG version check ──────────────────────────────────────────
        {
            use pms_storage::{DAG_VERSION, DagSemVer, VersionCheck, check_dag_compatibility};

            if is_fresh_db {
                // Nouvelle DB → écrire la version courante
                store
                    .set_dag_version(DAG_VERSION)
                    .await
                    .context("writing initial DAG version")?;
                tracing::info!(ledger = %def.id, dag_version = DAG_VERSION, "DAG version set (fresh DB)");
            } else {
                let stored_str = store
                    .get_dag_version()
                    .await
                    .context("reading DAG version")?;
                let stored = DagSemVer::parse(&stored_str)
                    .with_context(|| format!("invalid stored DAG version: '{stored_str}'"))?;
                let current = DagSemVer::parse(DAG_VERSION)
                    .expect("DAG_VERSION constant must be valid SemVer");

                match check_dag_compatibility(&stored, &current) {
                    VersionCheck::Compatible => {
                        if stored != current {
                            tracing::info!(
                                ledger = %def.id,
                                from = %stored,
                                to = %current,
                                "Auto-migrating DAG version (minor/patch)"
                            );
                            store
                                .set_dag_version(DAG_VERSION)
                                .await
                                .context("updating DAG version")?;
                        }
                    }
                    VersionCheck::Downgrade => {
                        anyhow::bail!(
                            "Ledger '{}': DAG version downgrade not supported. \
                             Stored: {}, Binary: {}. \
                             This data was created with a newer version. Please upgrade the software.",
                            def.id, stored, current
                        );
                    }
                    VersionCheck::MajorMismatch => {
                        anyhow::bail!(
                            "Ledger '{}': DAG version MAJOR mismatch. \
                             Stored: {}, Binary: {}. \
                             Breaking changes detected. Manual migration or fresh DB required.",
                            def.id, stored, current
                        );
                    }
                }
            }
        }

        // Bootstrap DAG from store (with capacity limit for RAM pruning)
        let dag = Arc::new(
            ConcurrentDag::bootstrap_from_store_with_capacity(
                &*store,
                max_dag_blocks,
                max_spent_outpoints,
            )
            .await
            .with_context(|| format!("bootstrap DAG for ledger '{}'", def.id))?,
        );
        tracing::info!(ledger = %def.id, blocks = dag.len(), max_dag_blocks, "DAG loaded");

        // ── UTXO LRU fallback closure (reads from RocksDB on cache miss) ──
        let store_for_fallback = store.clone();
        let utxo_fallback: UtxoFetcher = Arc::new(move |txid: &str, index: u32| {
            store_for_fallback
                .get_utxo(txid, index)
                .ok()
                .flatten()
                .map(|uv| pms_types::TxOutput {
                    address: uv.address,
                    amount: uv.amount,
                    asset_id: uv.asset_id,
                })
        });

        // Adapter + UTXO bootstrap from RocksDB utxo CF (authoritative, never pruned)
        let core_adapter = CoreAdapter::new(
            dag.clone(),
            store.clone(),
            max_utxos,
            Some(utxo_fallback),
        );
        {
            use std::sync::mpsc;

            // Stream UTXOs from RocksDB through a bounded channel — O(buffer) memory
            // instead of O(total_utxos). Avoids OOM on large UTXO sets.
            const STREAM_BUFFER: usize = 10_000;
            let (tx, rx) = mpsc::sync_channel(STREAM_BUFFER);

            let store_for_stream = store.clone();
            let ledger_id = def.id.clone();

            let producer = std::thread::Builder::new()
                .name(format!("utxo-stream-{}", def.id))
                .spawn(move || -> anyhow::Result<usize> {
                    store_for_stream
                        .stream_all_utxos(tx)
                        .with_context(|| format!("stream_all_utxos for ledger '{ledger_id}'"))
                })
                .with_context(|| format!("spawn UTXO stream thread for ledger '{}'", def.id))?;

            // Consume UTXOs as they arrive. rx.recv() blocks the current task,
            // which is acceptable during bootstrap: no concurrent async work,
            // and add().await resolves immediately (no lock contention).
            let mut utxo_count: usize = 0;
            while let Ok((txid, idx, uv)) = rx.recv() {
                let oid = pms_types::OutputId {
                    txid,
                    index: idx,
                };
                let txo = pms_types::TxOutput {
                    address: uv.address,
                    amount: uv.amount,
                    asset_id: uv.asset_id,
                };
                core_adapter.utxos.add(oid, txo).await;
                utxo_count += 1;
                if utxo_count % 100_000 == 0 {
                    tracing::info!(ledger = %def.id, utxo_count, "UTXO streaming progress...");
                }
            }

            // Join producer and propagate any RocksDB/parsing errors
            producer
                .join()
                .map_err(|e| anyhow::anyhow!("UTXO stream thread panicked: {e:?}"))?
                .with_context(|| {
                    format!("UTXO stream iteration failed for ledger '{}'", def.id)
                })?;

            // When all UTXOs fit in cache, rebuild indexes from shard data (defensive).
            // When eviction occurred, skip rebuild — add() already computed correct caches.
            if max_utxos == 0 || utxo_count <= max_utxos {
                core_adapter.utxos.rebuild_indexes().await;
            }

            let cached = core_adapter.utxos.total_len().await;
            tracing::info!(
                ledger = %def.id,
                utxos_total = utxo_count,
                utxos_cached = cached,
                max_utxos,
                "UTXO set bootstrapped from RocksDB (streaming)"
            );
        }

        let utxos = core_adapter.utxos.clone();
        let adapter: Arc<dyn NetDagAdapter> = core_adapter;

        Ok(Self {
            id: def.id.clone(),
            dag,
            utxos,
            store,
            adapter,
            def,
        })
    }
}
