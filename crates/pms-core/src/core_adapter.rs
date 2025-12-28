use std::sync::{Arc, Weak};
use tokio::sync::{Mutex, RwLock};
use pms_config::load_config;
use pms_errors::ValidationError;
use pms_storage::DagStorage;
use pms_utils::check_pow_leading_zero_bits;
use pms_wire::WireBlock;
use crate::{Dag, DagRef, ValidatePolicy};

/// CoreAdapter : colle la logique du DAG, du store, et du serveur réseau.
///
/// - `dag` : état local (RAM) protégé par `Mutex` car accédé en async.
/// - `store` : persistance (RocksDB, …).
/// - `server` : lien **faible** vers le serveur réseau pour éviter un cycle Arc.
pub struct CoreAdapter<S: DagStorage + Send + Sync + 'static> {
    /// DAG en RAM (cache + logique d’attache de parents, etc.)
    pub dag: Arc<Mutex<Dag>>,
    /// Stockage persistant.
    pub store: Arc<S>,
    /// Référence faible vers le serveur (faible -> pas de fuite si cycle).
    pub(crate) policy: ValidatePolicy,
}

impl<S: DagStorage + Send + Sync + 'static> CoreAdapter<S> {
    /// Étape 1/2 : construit l’adapter **sans** serveur attaché.
    ///
    /// On met `server` à `Weak::new()` ; il sera renseigné par `set_server` (étape 2/2).
    pub fn new(dag: Arc<Mutex<Dag>>, store: Arc<S>) -> Arc<Self> {
        let settings = load_config().expect("config");    // ou injecte depuis le main
        let mut p = ValidatePolicy::from_global_config();
        if settings.network.mode.is_non_prod() { p.min_parents_after_boot = 1; }
        Arc::new(Self { dag, store, policy: p })
    }

    pub fn new_with_policy(
        dag: Arc<Mutex<Dag>>,
        store: Arc<S>,
        mut policy: ValidatePolicy,
    ) -> Arc<Self> {
        // Garde ta logique actuelle de min_parents_after_boot, etc.
        let settings = load_config().expect("config");
        if settings.network.mode.is_non_prod() {
            policy.min_parents_after_boot = 1;
        }
        Arc::new(Self { dag, store, policy })
    }

    pub(crate) fn validate_wire_block_pow(&self, wb: &WireBlock) -> Result<(), ValidationError> {
        let bits = self.policy.min_pow_leading_zero_bits;

        if bits == 0 {
            return Ok(()); // PoW désactivé
        }

        if !check_pow_leading_zero_bits(&wb.id, bits) {
            return Err(ValidationError::InvalidDifficulty {
                id: wb.id.clone(),
                required_bits: bits,
            });
        }

        Ok(())
    }

    pub fn dag_ref(&self) -> DagRef {
        self.dag.clone()
    }
}