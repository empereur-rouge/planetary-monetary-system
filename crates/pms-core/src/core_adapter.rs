use crate::background_activity::spawn_activity_writer;
use crate::background_persist::{PersistJob, spawn_background_persist_with_activity};
use crate::concurrent_dag::ConcurrentDag;
use crate::utxo::{ShardedUtxoSet, UtxoFetcher};
use crate::{DagRef, ValidatePolicy};
use parking_lot::RwLock;
use pms_config::{load_config, Settings};
use pms_event::EventBus;
use pms_storage::coordinator_key_store::{CoordinatorKeyStorage, KeyRotationRecord};
use pms_storage::{ComplianceStorage, DagStorage, NftStorage};
use pms_wire::WireMeta;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;

/// In-memory snapshot of the coordinator key rotation state.
///
/// Built once at adapter construction (bootstrap key + replay of
/// `CoordinatorKeyStorage::list_key_rotations`) and refreshed whenever
/// a `CoordinatorKeyRotate` block is successfully persisted. The
/// validator consults this struct on every block to decide whether the
/// signer is currently authorised.
#[derive(Debug, Clone, Default)]
pub struct KeyRotationState {
    /// The key shipped in `[validation].coordinator_public_key` at boot.
    /// `None` in pure dev mode.
    pub bootstrap_pk: Option<String>,
    /// Recorded rotations, oldest first. Each one's `new_pk` becomes
    /// the "current" coordinator key once applied.
    pub rotations: Vec<KeyRotationRecord>,
}

impl KeyRotationState {
    /// The coordinator key that holds mint authority RIGHT NOW: the
    /// most recent rotation's `new_pk`, or the bootstrap key if no
    /// rotation has ever landed. `None` only in dev (no bootstrap key
    /// configured).
    pub fn current_pk(&self) -> Option<&str> {
        self.rotations
            .last()
            .map(|r| r.new_pk.as_str())
            .or(self.bootstrap_pk.as_deref())
    }

    /// Set of keys currently allowed to sign blocks: `current_pk` plus
    /// any rotation's `old_pk` whose grace window hasn't expired yet
    /// at the supplied wall-clock. Used by the single-writer signer
    /// check on every block.
    pub fn accepted_signer_keys(&self, now_ms: i64) -> HashSet<String> {
        let mut set: HashSet<String> = HashSet::new();
        if let Some(c) = self.current_pk() {
            set.insert(c.to_string());
        }
        for r in &self.rotations {
            // The current pk is `rotations.last().new_pk`; every other
            // rotation's `old_pk` is a candidate for the grace window.
            // We include each one whose grace hasn't expired — the
            // overlap of two consecutive rotations' grace windows is
            // intentional, it widens the tolerance during back-to-back
            // rotations.
            if r.old_key_in_grace(now_ms) {
                set.insert(r.old_pk.clone());
            }
        }
        set
    }
}

#[cfg(test)]
mod key_rotation_state_tests {
    use super::*;

    fn rec(old: &str, new: &str, ts: i64, grace: u64) -> KeyRotationRecord {
        KeyRotationRecord {
            old_pk: old.into(),
            new_pk: new.into(),
            applied_at_block_id: format!("blk-{ts}"),
            applied_at_ts_ms: ts,
            grace_window_seconds: grace,
        }
    }

    #[test]
    fn empty_state_falls_back_to_bootstrap() {
        let s = KeyRotationState {
            bootstrap_pk: Some("boot".into()),
            rotations: vec![],
        };
        assert_eq!(s.current_pk(), Some("boot"));
        let set = s.accepted_signer_keys(1_000_000);
        println!("empty: {set:?}");
        assert_eq!(set.len(), 1);
        assert!(set.contains("boot"));
    }

    #[test]
    fn current_follows_latest_rotation() {
        let s = KeyRotationState {
            bootstrap_pk: Some("boot".into()),
            rotations: vec![
                rec("boot", "v2", 1_000, 60), // grace 60s
                rec("v2", "v3", 5_000, 30),   // grace 30s
            ],
        };
        // current = "v3"
        assert_eq!(s.current_pk(), Some("v3"));

        // At t=6_000ms (1s after second rotation):
        //   - "v3" current ✓
        //   - "v2" still in grace (5_000 + 30_000 = 35_000) ✓
        //   - "boot" still in grace (1_000 + 60_000 = 61_000) ✓
        let set = s.accepted_signer_keys(6_000);
        println!("at t=6_000: {set:?}");
        assert!(set.contains("v3"));
        assert!(set.contains("v2"));
        assert!(set.contains("boot"));
        assert_eq!(set.len(), 3);

        // At t=70_000ms — both grace windows expired.
        let later = s.accepted_signer_keys(70_000);
        println!("at t=70_000: {later:?}");
        assert_eq!(later.len(), 1);
        assert!(later.contains("v3"));
    }

    #[test]
    fn atomic_rotation_revokes_old_pk_immediately() {
        let s = KeyRotationState {
            bootstrap_pk: Some("boot".into()),
            rotations: vec![rec("boot", "v2", 1_000, 0)], // grace 0
        };
        // grace=0 means old_pk is rejected starting from the moment
        // of rotation. `accepted` at the rotation timestamp is just
        // {current}.
        let set = s.accepted_signer_keys(1_000);
        println!("atomic at t=1_000: {set:?}");
        assert_eq!(set.len(), 1);
        assert!(set.contains("v2"));
        assert!(!set.contains("boot"));
    }
}

/// CoreAdapter : colle la logique du DAG, du store, et du serveur réseau.
///
/// - `dag` : DAG concurrent (lock-free) pour haute performance (IOTA-like).
/// - `store` : persistance (RocksDB, …).
/// - `server` : lien **faible** vers le serveur réseau pour éviter un cycle Arc.
pub struct CoreAdapter<
    S: DagStorage + NftStorage + ComplianceStorage + CoordinatorKeyStorage + Send + Sync + 'static,
> {
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
    /// Cached settings. Loaded once at construction to avoid re-reading the
    /// config file from disk on every persist_block() call.
    pub(crate) settings: Settings,
    /// Cached wire metadata (derived from settings).
    pub(crate) wire_meta: WireMeta,
    /// In-memory snapshot of the coordinator key rotation state (audit
    /// item 8, v0.7.4). Read on every persist to decide whether the
    /// block signer is authorised; refreshed whenever a
    /// `CoordinatorKeyRotate` block lands.
    pub(crate) key_rotation_state: Arc<RwLock<KeyRotationState>>,
}

impl<
    S: DagStorage + NftStorage + ComplianceStorage + CoordinatorKeyStorage + Send + Sync + 'static,
> CoreAdapter<S>
{
    /// Étape 1/2 : construit l'adapter **sans** serveur attaché.
    ///
    /// On met `server` à `Weak::new()` ; il sera renseigné par `set_server` (étape 2/2).
    pub fn new(
        dag: Arc<ConcurrentDag>,
        store: Arc<S>,
        max_utxos: usize,
        utxo_fallback: Option<UtxoFetcher>,
    ) -> Arc<Self> {
        let settings = load_config().expect("config"); // ou injecte depuis le main
        let wire_meta = WireMeta::from(&settings);
        let mut p = ValidatePolicy::from_global_config();
        if settings.network.mode.is_non_prod() {
            p.min_parents_after_boot = 1;
        }
        // Force l'utilisation du ShardedUtxoSet (Phase 4)
        p.skip_utxo_checks = true;

        // Spawn the activity writer (background task that drains a
        // dedicated channel and writes addr_activity / addr_type_activity /
        // activity_items CFs out-of-band). The buffer is intentionally
        // generous — activity events are non-critical, so we'd rather
        // absorb spikes than drop dashboard rows during prod load.
        let (activity_tx, _activity_handle) =
            spawn_activity_writer(store.clone(), 50_000);

        // Spawn background persist task (buffer 2K blocks — smaller buffer limits
        // RAM usage under write pressure; back-pressure in persist.rs ensures no drops).
        // The persist consumer forwards each freshly-persisted block to the
        // activity writer via `activity_tx` so the dashboard's history is
        // populated without blocking the critical persist `db.write()` slot.
        let (persist_tx, _handle) = spawn_background_persist_with_activity(
            store.clone(),
            2_000,
            activity_tx,
        );

        // Event bus avec capacité 4096 (haut débit)
        let event_bus = EventBus::new(4096);

        // Coordinator key rotation cache: bootstrap pk from policy +
        // replay every persisted rotation. Failures during the replay
        // are logged but don't abort startup — a fresh DB has no
        // history and the default empty Vec is correct.
        let key_rotation_state = Self::initial_key_rotation_state(&p, &store);

        Arc::new(Self {
            dag,
            store,
            policy: p,
            utxos: Arc::new(ShardedUtxoSet::new(max_utxos, utxo_fallback)),
            persist_tx,
            event_bus,
            settings,
            wire_meta,
            key_rotation_state,
        })
    }

    fn initial_key_rotation_state(
        policy: &ValidatePolicy,
        store: &Arc<S>,
    ) -> Arc<RwLock<KeyRotationState>> {
        let bootstrap_pk = policy.coordinator_public_key.clone();
        let rotations = match store.list_key_rotations() {
            Ok(rs) => rs,
            Err(e) => {
                tracing::warn!(
                    target = "key_rotation",
                    error = %e,
                    "Failed to load coordinator key rotation history at boot — \
                     starting with empty history. Manual /admin/refresh-key-rotation \
                     can recover once the underlying error is fixed."
                );
                Vec::new()
            }
        };
        if !rotations.is_empty() {
            tracing::info!(
                target = "key_rotation",
                count = rotations.len(),
                "Loaded coordinator key rotation history"
            );
        }
        Arc::new(RwLock::new(KeyRotationState {
            bootstrap_pk,
            rotations,
        }))
    }

    /// Reload the rotation cache from storage. Called after a
    /// `CoordinatorKeyRotate` block successfully persists. Cheap — the
    /// CF holds at most a few dozen rows in any realistic deployment.
    pub(crate) fn refresh_key_rotation_state(&self) {
        match self.store.list_key_rotations() {
            Ok(rotations) => {
                let mut state = self.key_rotation_state.write();
                state.rotations = rotations;
            }
            Err(e) => {
                tracing::error!(
                    target = "key_rotation",
                    error = %e,
                    "Failed to refresh coordinator key rotation state — \
                     in-RAM cache is now stale until next restart. \
                     Investigate immediately."
                );
            }
        }
    }

    pub fn new_with_policy(
        dag: Arc<ConcurrentDag>,
        store: Arc<S>,
        mut policy: ValidatePolicy,
        max_utxos: usize,
        utxo_fallback: Option<UtxoFetcher>,
    ) -> Arc<Self> {
        eprintln!(
            "[ADAPTER] new_with_policy entry: enforce_parents={}",
            policy.enforce_parent_existence
        );
        // Garde ta logique actuelle de min_parents_after_boot, etc.
        let settings = load_config().expect("config");
        let wire_meta = WireMeta::from(&settings);
        if settings.network.mode.is_non_prod() {
            policy.min_parents_after_boot = 1;
        }

        // Spawn the activity writer (background task that drains a
        // dedicated channel and writes addr_activity / addr_type_activity /
        // activity_items CFs out-of-band). The buffer is intentionally
        // generous — activity events are non-critical, so we'd rather
        // absorb spikes than drop dashboard rows during prod load.
        let (activity_tx, _activity_handle) =
            spawn_activity_writer(store.clone(), 50_000);

        // Spawn background persist task (buffer 2K blocks — smaller buffer limits
        // RAM usage under write pressure; back-pressure in persist.rs ensures no drops).
        // The persist consumer forwards each freshly-persisted block to the
        // activity writer via `activity_tx` so the dashboard's history is
        // populated without blocking the critical persist `db.write()` slot.
        let (persist_tx, _handle) = spawn_background_persist_with_activity(
            store.clone(),
            2_000,
            activity_tx,
        );

        // Event bus avec capacité 4096
        let event_bus = EventBus::new(4096);

        let key_rotation_state = Self::initial_key_rotation_state(&policy, &store);

        Arc::new(Self {
            dag,
            store,
            policy,
            utxos: Arc::new(ShardedUtxoSet::new(max_utxos, utxo_fallback)),
            persist_tx,
            event_bus,
            settings,
            wire_meta,
            key_rotation_state,
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
