//! Définition des types d'événements PMS.
//!
//! L'enum `PmsEvent` est conçu pour être extensible. Chaque nouveau type
//! d'événement (ex: nouveau type de smart contract) sera ajouté comme
//! un nouveau variant.

use pms_types_nft::NftAction;
use serde::{Deserialize, Serialize};

/// Événements émis par le système PMS.
///
/// # Extensibilité
/// Pour ajouter un nouvel événement :
/// 1. Ajouter un nouveau variant à cet enum
/// 2. Mettre à jour les handlers dans les modules consommateurs
/// 3. Émettre l'événement depuis `pms-core` au bon moment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PmsEvent {
    // ═══════════════════════════════════════════════════════════════════
    // NFT EVENTS (réutilise NftAction de pms-types-nft)
    // ═══════════════════════════════════════════════════════════════════
    /// Événement NFT (Mint, Transfer, Use, Burn).
    ///
    /// Encapsule une `NftAction` avec le `block_id` du bloc validé.
    Nft {
        /// ID du bloc contenant l'action NFT
        block_id: String,
        /// L'action NFT effectuée
        action: NftAction,
    },

    // ═══════════════════════════════════════════════════════════════════
    // SMART CONTRACT EVENTS (FUTURE)
    // ═══════════════════════════════════════════════════════════════════
    /// Un smart contract a été exécuté avec succès.
    ContractFulfilled {
        block_id: String,
        contract_id: String,
        result: String,
    },

    /// Un smart contract a échoué.
    ContractFailed {
        block_id: String,
        contract_id: String,
        error: String,
    },

    // ═══════════════════════════════════════════════════════════════════
    // SYSTEM EVENTS
    // ═══════════════════════════════════════════════════════════════════
    /// Un bloc Milestone a été confirmé.
    MilestoneConfirmed {
        block_id: String,
        approved_blocks: Vec<String>,
    },

    /// Nouveau bloc ajouté au DAG (informatif, haut débit).
    BlockAdded { block_id: String },

    /// Récompense distribuée à un nœud (loyer de nœud).
    NodeRewardDistributed {
        /// Clé publique du nœud
        node_pk: String,
        /// Adresse de récompense
        address: String,
        /// Montant en satoshis
        amount_sats: u64,
        /// ID du Milestone qui a déclenché la distribution
        milestone_id: String,
    },

    // ═══════════════════════════════════════════════════════════════════
    // ACTIVITY STREAM EVENTS
    // ═══════════════════════════════════════════════════════════════════
    /// Bloc persisté dans le DAG avec adresses pré-calculées.
    /// Utilisé par le SSE `/v1/wallet/{address}/activity/stream` pour filtrer
    /// en mémoire sans accès DB.
    BlockPersisted {
        block_id: String,
        ts_ms: i64,
        /// Type de payload : "Mint", "TxUtxo", "Reward", "Nft", etc.
        payload_type: String,
        /// Toutes les adresses impliquées (outputs, sender, fee recipients, etc.)
        involved_addresses: Vec<String>,
        /// Payload JSON sérialisé pour classification sans re-fetch DB
        payload_json: String,
    },
}

impl PmsEvent {
    /// Retourne le type de l'événement sous forme de chaîne.
    pub fn event_type(&self) -> &'static str {
        match self {
            PmsEvent::Nft { action, .. } => match action {
                NftAction::Mint { .. } => "nft_minted",
                NftAction::Transfer { .. } => "nft_transferred",
                NftAction::Use { .. } => "nft_used",
                NftAction::Burn { .. } => "nft_burned",
                NftAction::BatchBurn { .. } => "nft_batch_burned",
            },
            PmsEvent::ContractFulfilled { .. } => "contract_fulfilled",
            PmsEvent::ContractFailed { .. } => "contract_failed",
            PmsEvent::MilestoneConfirmed { .. } => "milestone_confirmed",
            PmsEvent::BlockAdded { .. } => "block_added",
            PmsEvent::NodeRewardDistributed { .. } => "node_reward_distributed",
            PmsEvent::BlockPersisted { .. } => "block_persisted",
        }
    }

    /// Retourne le block_id associé à l'événement.
    pub fn block_id(&self) -> &str {
        match self {
            PmsEvent::Nft { block_id, .. } => block_id,
            PmsEvent::ContractFulfilled { block_id, .. } => block_id,
            PmsEvent::ContractFailed { block_id, .. } => block_id,
            PmsEvent::MilestoneConfirmed { block_id, .. } => block_id,
            PmsEvent::BlockAdded { block_id } => block_id,
            PmsEvent::NodeRewardDistributed { milestone_id, .. } => milestone_id,
            PmsEvent::BlockPersisted { block_id, .. } => block_id,
        }
    }

    /// Helper pour créer un événement NFT.
    pub fn nft(block_id: String, action: NftAction) -> Self {
        PmsEvent::Nft { block_id, action }
    }
}
