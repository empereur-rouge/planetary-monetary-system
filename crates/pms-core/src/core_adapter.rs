use crate::background_persist::{PersistJob, spawn_background_persist};
use crate::concurrent_dag::ConcurrentDag;
use crate::utxo::ShardedUtxoSet;
use crate::{DagRef, ValidatePolicy};
use pms_config::load_config;
use pms_event::EventBus;
use pms_storage::{ComplianceStorage, DagStorage, NftStorage};
use std::sync::Arc;
use tokio::sync::mpsc;

/// CoreAdapter : colle la logique du DAG, du store, et du serveur réseau.
///
/// - `dag` : DAG concurrent (lock-free) pour haute performance (IOTA-like).
/// - `store` : persistance (RocksDB, …).
/// - `server` : lien **faible** vers le serveur réseau pour éviter un cycle Arc.
pub struct CoreAdapter<S: DagStorage + NftStorage + ComplianceStorage + Send + Sync + 'static> {
    /// DAG concurrent lock-free (IOTA-like architecture)
    pub dag: Arc<ConcurrentDag>,
    /// Stockage persistant.
    pub store: Arc<S>,
    /// Référence faible vers le serveur (faible -> pas de fuite si cycle).
    pub(crate) policy: ValidatePolicy,
    /// UTXO Set partitionné pour accès concurrent (Sharding Phase 4)
    pub utxos: Arc<ShardedUtxoSet>,
    /// Channel pour envoyer les blocs à persister en background
    pub(crate) persist_tx: mpsc::Sender<PersistJob>,
    /// Event bus pour émettre les événements (NFT, Milestone, etc.)
    pub event_bus: EventBus,
}

impl<S: DagStorage + NftStorage + ComplianceStorage + Send + Sync + 'static> CoreAdapter<S> {
    /// Étape 1/2 : construit l'adapter **sans** serveur attaché.
    ///
    /// On met `server` à `Weak::new()` ; il sera renseigné par `set_server` (étape 2/2).
    pub fn new(dag: Arc<ConcurrentDag>, store: Arc<S>) -> Arc<Self> {
        let settings = load_config().expect("config"); // ou injecte depuis le main
        let mut p = ValidatePolicy::from_global_config();
        if settings.network.mode.is_non_prod() {
            p.min_parents_after_boot = 1;
        }
        // Force l'utilisation du ShardedUtxoSet (Phase 4)
        p.skip_utxo_checks = true;

        // Spawn background persist task (buffer 10k blocks)
        let (persist_tx, _handle) = spawn_background_persist(store.clone(), 10_000);

        // Event bus avec capacité 4096 (haut débit)
        let event_bus = EventBus::new(4096);

        Arc::new(Self {
            dag,
            store,
            policy: p,
            utxos: Arc::new(ShardedUtxoSet::new()),
            persist_tx,
            event_bus,
        })
    }

    pub fn new_with_policy(
        dag: Arc<ConcurrentDag>,
        store: Arc<S>,
        mut policy: ValidatePolicy,
    ) -> Arc<Self> {
        eprintln!(
            "[ADAPTER] new_with_policy entry: enforce_parents={}",
            policy.enforce_parent_existence
        );
        // Garde ta logique actuelle de min_parents_after_boot, etc.
        let settings = load_config().expect("config");
        if settings.network.mode.is_non_prod() {
            policy.min_parents_after_boot = 1;
        }

        // Spawn background persist task (buffer 10k blocks)
        let (persist_tx, _handle) = spawn_background_persist(store.clone(), 10_000);

        // Event bus avec capacité 4096
        let event_bus = EventBus::new(4096);

        Arc::new(Self {
            dag,
            store,
            policy,
            utxos: Arc::new(ShardedUtxoSet::new()),
            persist_tx,
            event_bus,
        })
    }

    pub fn dag_ref(&self) -> DagRef {
        self.dag.clone()
    }

    pub fn policy(&self) -> &ValidatePolicy {
        &self.policy
    }

    /// Reconstruit le ShardedUtxoSet depuis le DAG en mémoire.
    /// Algorithme 2-passes robuste au désordre :
    /// 1. Collecter tous les outpoints consommés (inputs) par toutes les tx.
    /// 2. Parcourir tous les outputs créés : si non consommés, ajouter au UTXO set.
    pub async fn bootstrap_utxos(&self) -> anyhow::Result<()> {
        // No lock needed for ConcurrentDag (concurrent iteration via DashMap)

        // Pass 1: Collect all Spends
        let mut spent = std::collections::HashSet::new();
        // Since blocks is a DashMap, we iterate over references
        for r in &self.dag.blocks {
            let b = r.value();
            if let Some(p) = &b.payload {
                if let pms_types::PayloadEnvelope::Plain(pms_types::PlainPayload::TxUtxo(tx)) = p {
                    for inp in &tx.inputs {
                        // On stocke txid+index
                        spent.insert((inp.out.txid.clone(), inp.out.index));
                    }
                }
                if let pms_types::PayloadEnvelope::Plain(pms_types::PlainPayload::BridgeLock {
                    inputs,
                    ..
                }) = p
                {
                    for inp in inputs {
                        spent.insert((inp.out.txid.clone(), inp.out.index));
                    }
                }
                if let pms_types::PayloadEnvelope::Plain(pms_types::PlainPayload::Seize {
                    inputs,
                    ..
                }) = p
                {
                    for inp in inputs {
                        spent.insert((inp.out.txid.clone(), inp.out.index));
                    }
                }
                if let pms_types::PayloadEnvelope::Plain(pms_types::PlainPayload::Reverse {
                    inputs,
                    ..
                }) = p
                {
                    for inp in inputs {
                        spent.insert((inp.out.txid.clone(), inp.out.index));
                    }
                }
            }
        }

        // Pass 2: Collect Unspent Outputs
        for r in &self.dag.blocks {
            let b = r.value();
            let outputs_with_base_idx: Option<(Vec<pms_types::TxOutput>, u32)> = match &b.payload {
                Some(pms_types::PayloadEnvelope::Plain(pms_types::PlainPayload::Mint {
                    outputs,
                })) => Some((outputs.clone(), 0)),
                Some(pms_types::PayloadEnvelope::Plain(pms_types::PlainPayload::TxUtxo(tx))) => {
                    Some((tx.outputs.clone(), 0))
                }
                Some(pms_types::PayloadEnvelope::Plain(pms_types::PlainPayload::BridgeMint {
                    outputs,
                    ..
                })) => Some((outputs.clone(), 0)),
                Some(pms_types::PayloadEnvelope::Plain(pms_types::PlainPayload::Seize {
                    outputs,
                    ..
                })) => Some((outputs.clone(), 0)),
                Some(pms_types::PayloadEnvelope::Plain(pms_types::PlainPayload::Reverse {
                    outputs,
                    ..
                })) => Some((outputs.clone(), 0)),
                _ => None,
            };

            if let Some((outs, base_idx)) = outputs_with_base_idx {
                for (i, out) in outs.into_iter().enumerate() {
                    let idx = base_idx + i as u32;
                    let out_id_tuple = (b.id.clone(), idx);

                    // Si pas dépensé, on ajoute au set
                    if !spent.contains(&out_id_tuple) {
                        // Convert tuple -> OutputId
                        let oid = pms_types::OutputId {
                            txid: b.id.clone(),
                            index: idx,
                        };
                        self.utxos.add(oid, out).await;
                    }
                }
            }
        }

        // Rebuild address index + supply cache in a single O(n) pass
        self.utxos.rebuild_indexes().await;

        let total = self.utxos.total_len().await;
        println!(
            "[CoreAdapter] Bootstrapped UTXO set: {} unspent outputs",
            total
        );
        Ok(())
    }
}
