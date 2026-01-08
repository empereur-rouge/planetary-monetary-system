//! Helper pour créer un coordinateur de test.
//!
//! Le coordinateur est un nœud spécial autorisé à :
//! - Émettre des Milestones
//! - Modifier la RuntimeConfig
//! - Distribuer les récompenses aux nœuds

use pms_types::{PayloadEnvelope, PlainPayload};
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;

/// Coordinateur de test avec wallet pré-configuré.
pub struct TestCoordinator {
    /// Le wallet du coordinateur
    pub wallet: Wallet,
    /// Clé publique hex du coordinateur
    pub public_key: String,
}

impl TestCoordinator {
    /// Crée un nouveau coordinateur de test.
    ///
    /// Utilise une seed fixe pour garantir la reproductibilité.
    pub fn new() -> Self {
        // Seed simple et prévisible pour les tests
        let seed: [u8; 32] = [0xAB; 32];
        let wallet = Wallet::from_seed(&seed, None).expect("Coordinator wallet creation");
        let public_key = wallet.public_key_hex.clone();

        Self { wallet, public_key }
    }

    /// Crée un coordinateur avec une seed personnalisée.
    pub fn with_seed(seed: &[u8; 32]) -> Self {
        let wallet = Wallet::from_seed(seed, None).expect("Coordinator wallet creation");
        let public_key = wallet.public_key_hex.clone();

        Self { wallet, public_key }
    }

    /// Crée un WireBlock Milestone signé par ce coordinateur.
    pub fn forge_milestone(
        &self,
        parents: Vec<String>,
        meta: &WireMeta,
        approved: Vec<String>,
        distribute_rewards: bool,
    ) -> pms_wire::WireBlock {
        let payload = PayloadEnvelope::Plain(PlainPayload::Milestone {
            approved,
            distribute_node_rewards: distribute_rewards,
        });

        crate::forge_signed_wire_block_for_test(parents, meta, &self.wallet, 0, Some(payload))
    }

    /// Crée un WireBlock ConfigUpdate signé par ce coordinateur.
    pub fn forge_config_update(
        &self,
        parents: Vec<String>,
        meta: &WireMeta,
        update: pms_config::ConfigUpdate,
    ) -> pms_wire::WireBlock {
        let payload = PayloadEnvelope::Plain(PlainPayload::ConfigUpdate(update));

        crate::forge_signed_wire_block_for_test(parents, meta, &self.wallet, 0, Some(payload))
    }
}

impl Default for TestCoordinator {
    fn default() -> Self {
        Self::new()
    }
}
