//! Types de gouvernance timelock (plan §4 / `pms-spec-governance-timelock.md`).
//!
//! Un changement de paramètre gouverné passe par un **processus** : une
//! `GovernanceProposal` (annonce, ancrée DAG, timelockée) → délai incompressible
//! par palier d'impact → un `GovernanceEnact` (effet, ancré DAG) qui applique le
//! `ConfigUpdate`. Ici vivent les types partagés (palier, statut, enregistrement
//! stocké). Les payloads DAG sont dans `pms-types-payload`.

use crate::ConfigUpdate;
use serde::{Deserialize, Serialize};

/// Palier d'impact d'un changement gouverné — détermine le timelock (plan §4.2).
///
/// L'ordre des variants (`Operator < Policy < Constitution`) est **signifiant** :
/// `derive(PartialOrd, Ord)` l'utilise pour la comparaison `tier >= palier-min`
/// ([`crate::governance_policy::min_tier`]). Ne jamais réordonner les variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GovernanceTier {
    /// Calibrage opérationnel (fees, anti-spam) — délai court.
    Operator,
    /// Politique (ouvrir une voie, burn rate, taux cible) — délai moyen.
    Policy,
    /// Constitution (le couloir d'émission, le pouvoir de mint, qui gouverne) —
    /// délai long, la classe « règle inviolable ».
    Constitution,
}

impl GovernanceTier {
    /// Durée par défaut du timelock (ms) — 7 / 15 / 45 jours (plan §4.2 ✅).
    /// Surchargeable par config (testnet raccourci) en phase ultérieure ; les
    /// proposants calculent `enact_after` à partir de cette durée.
    pub fn default_duration_ms(&self) -> u64 {
        const DAY_MS: u64 = 86_400_000;
        match self {
            GovernanceTier::Operator => 7 * DAY_MS,
            GovernanceTier::Policy => 15 * DAY_MS,
            GovernanceTier::Constitution => 45 * DAY_MS,
        }
    }

    /// Label stable (logs, métriques, API).
    pub fn as_str(&self) -> &'static str {
        match self {
            GovernanceTier::Operator => "operator",
            GovernanceTier::Policy => "policy",
            GovernanceTier::Constitution => "constitution",
        }
    }
}

/// Statut d'une proposition dans son cycle de vie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernanceStatus {
    /// Annoncée, en attente d'expiration du timelock.
    Pending,
    /// Timelock expiré + appliquée (le `ConfigUpdate` a pris effet).
    Enacted,
    /// Annulée pendant le timelock (jamais appliquée).
    Cancelled,
}

impl GovernanceStatus {
    /// Label stable.
    pub fn as_str(&self) -> &'static str {
        match self {
            GovernanceStatus::Pending => "pending",
            GovernanceStatus::Enacted => "enacted",
            GovernanceStatus::Cancelled => "cancelled",
        }
    }
}

/// Enregistrement persisté d'une proposition (CF `governance_proposals`).
///
/// Construit à la persistance d'un bloc `GovernanceProposal` (statut `Pending`),
/// muté à `Enacted` / `Cancelled` par les blocs `GovernanceEnact` /
/// `GovernanceCancel`. Source de vérité requêtable pour
/// `GET /v1/governance/{pending,history}` et pour la validation de l'enact
/// (timelock écoulé).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GovernanceProposalRecord {
    /// `SHA-256(update + tier + announced_at)` — déterministe.
    pub proposal_id: String,
    /// Le changement de config à appliquer à l'enact.
    pub update: ConfigUpdate,
    /// Palier d'impact (→ durée du timelock).
    pub tier: GovernanceTier,
    /// Justification publique de la proposition.
    pub reason: String,
    /// Horodatage de l'annonce (ms) — timestamp du bloc proposal.
    pub announced_at_ms: u64,
    /// Effet autorisé à partir de cet instant (ms) = `announced_at + durée(tier)`.
    /// **Invariant timelock** : un enact à `now < enact_after_ms` est REJETÉ.
    pub enact_after_ms: u64,
    /// Statut courant.
    pub status: GovernanceStatus,
}
