use anyhow::{Context, Result};
use pms_config::LedgerDef;
use pms_core::CoreAdapter;
use pms_core::concurrent_dag::ConcurrentDag;
use pms_core::utxo::ShardedUtxoSet;
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
    ) -> Result<Self> {
        let tip_limit = def.tip_limit.unwrap_or(global_tip_limit);

        // Crée un RocksStore pointant vers la DB partagée avec ce prefix
        let store = Arc::new(RocksStore::from_shared_db(
            shared_db,
            &def.prefix,
            tip_limit,
            None,
        ));

        if let Err(e) = store.ensure_schema().await {
            tracing::warn!(ledger = %def.id, "ensure_schema: {e}");
        }

        // Genesis block si DB vide pour ce prefix
        let ids = store.all_block_ids().await.context("listing block IDs")?;

        if ids.is_empty() {
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

        // Adapter + UTXO bootstrap from RocksDB utxo CF (authoritative, never pruned)
        let core_adapter = CoreAdapter::new(dag.clone(), store.clone());
        {
            let all_utxos = store
                .iter_all_utxos()
                .with_context(|| format!("iter_all_utxos for ledger '{}'", def.id))?;
            for (txid, idx, uv) in &all_utxos {
                let oid = pms_types::OutputId {
                    txid: txid.clone(),
                    index: *idx,
                };
                let txo = pms_types::TxOutput {
                    address: uv.address.clone(),
                    amount: uv.amount.clone(),
                    asset_id: uv.asset_id.clone(),
                };
                core_adapter.utxos.add(oid, txo).await;
            }
            core_adapter.utxos.rebuild_indexes().await;
            tracing::info!(
                ledger = %def.id,
                utxos = all_utxos.len(),
                "UTXO set bootstrapped from RocksDB"
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
