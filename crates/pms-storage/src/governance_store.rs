//! Trait de persistance des propositions de gouvernance timelock (plan §4).
//!
//! Implémenté par `RocksStore` (CF `governance_proposals`). Fait partie de
//! [`EngineStorage`](crate::EngineStorage) pour être appelable depuis le hot
//! path persist (`pms-core`) qui ne connaît que le `S: EngineStorage` générique.

use anyhow::Result;
use pms_config::{GovernanceProposalRecord, GovernanceStatus};

/// Stockage des propositions de gouvernance (proposal_id → record).
pub trait GovernanceStorage: Send + Sync {
    /// Enregistre (ou écrase) une proposition.
    fn put_governance_proposal(&self, record: &GovernanceProposalRecord) -> Result<()>;

    /// Récupère une proposition par id (`None` si inconnue).
    fn get_governance_proposal(&self, proposal_id: &str)
    -> Result<Option<GovernanceProposalRecord>>;

    /// Mute le statut d'une proposition existante (erreur si inconnue).
    fn set_governance_status(&self, proposal_id: &str, status: GovernanceStatus) -> Result<()>;

    /// Liste toutes les propositions (tous statuts).
    fn list_governance_proposals(&self) -> Result<Vec<GovernanceProposalRecord>>;
}
