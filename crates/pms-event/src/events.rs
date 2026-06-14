//! Définition des types d'événements PMS.
//!
//! L'enum `PmsEvent` est conçu pour être extensible. Chaque nouveau type
//! d'événement (ex: nouveau type de smart contract) sera ajouté comme
//! un nouveau variant.

use pms_types_nft::{NftAction, NftMetadata};
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

    /// NFT burn traité avec succès — enrichi avec les métadonnées pré-fetchées.
    ///
    /// Émis par les handlers burn AVANT `apply_action()` (qui supprime le `block_id`
    /// du NFT, rendant les métadonnées irrécupérables). Le listener contrat
    /// consomme cet événement pour évaluer les contrats déclaratifs.
    NftBurnProcessed {
        /// ID du bloc contenant l'action burn
        block_id: String,
        /// ID du ledger où le burn a eu lieu
        ledger_id: String,
        /// Adresse bech32 du burner
        burner_address: String,
        /// IDs des tokens brûlés
        token_ids: Vec<String>,
        /// Métadonnées pré-fetchées (pour `AttributeFormula`)
        metadata: Option<NftMetadata>,
    },

    /// Burn de **token fongible** traité avec succès (plan §3.1, voie B).
    ///
    /// Émis par le handler de burn APRÈS persist du bloc `TokenBurn`. Le listener
    /// contrat le consomme pour évaluer les contrats `OnTokenBurn{asset_id}`
    /// (`evaluate_token_burn` → mint de PMS natif au taux R sous budget). Analogue
    /// fongible de [`PmsEvent::NftBurnProcessed`].
    TokenBurnProcessed {
        /// ID du bloc contenant le `TokenBurn`.
        block_id: String,
        /// ID du ledger où le burn a eu lieu.
        ledger_id: String,
        /// Adresse bech32 du burner (bénéficiaire d'un éventuel mint).
        burner_address: String,
        /// Asset brûlé (`None` = PMS natif).
        asset_id: Option<String>,
        /// Montant brûlé (string décimale).
        amount: String,
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
            PmsEvent::NftBurnProcessed { .. } => "nft_burn_processed",
            PmsEvent::TokenBurnProcessed { .. } => "token_burn_processed",
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
            PmsEvent::NftBurnProcessed { block_id, .. } => block_id,
            PmsEvent::TokenBurnProcessed { block_id, .. } => block_id,
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

    /// Helper pour créer un événement de burn NFT traité (enrichi avec métadonnées).
    pub fn nft_burn_processed(
        block_id: String,
        ledger_id: String,
        burner_address: String,
        token_ids: Vec<String>,
        metadata: Option<NftMetadata>,
    ) -> Self {
        PmsEvent::NftBurnProcessed {
            block_id,
            ledger_id,
            burner_address,
            token_ids,
            metadata,
        }
    }

    /// Helper pour créer un événement de burn de token fongible traité (voie B).
    pub fn token_burn_processed(
        block_id: String,
        ledger_id: String,
        burner_address: String,
        asset_id: Option<String>,
        amount: String,
    ) -> Self {
        PmsEvent::TokenBurnProcessed {
            block_id,
            ledger_id,
            burner_address,
            asset_id,
            amount,
        }
    }
}
