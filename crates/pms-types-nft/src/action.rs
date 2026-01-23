//! Actions possibles sur un NFT.
//!
//! Les actions sont utilisées dans les payloads de blocs pour modifier
//! l'état des NFTs sur la blockchain.

use crate::NftMetadata;
use serde::{Deserialize, Serialize};

/// Action effectuée sur un NFT.
///
/// Chaque action correspond à un type d'opération qui sera validée
/// par le nœud et émise comme événement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum NftAction {
    /// Création d'un nouveau NFT.
    ///
    /// Le `creator` devient également le `owner` initial.
    Mint {
        /// Identifiant unique du nouveau token
        token_id: String,
        /// Adresse du créateur/propriétaire initial
        creator: String,
        /// Métadonnées du NFT
        metadata: NftMetadata,
    },

    /// Transfert d'un NFT à un nouveau propriétaire.
    ///
    /// Si `new_owner_x25519_pubkey` est fourni, le coordinateur re-chiffrera
    /// les métadonnées pour le nouveau propriétaire.
    Transfer {
        /// ID du token à transférer
        token_id: String,
        /// Adresse de l'ancien propriétaire (doit signer la tx)
        from: String,
        /// Adresse du nouveau propriétaire
        to: String,
        /// Clé publique X25519 du nouveau propriétaire pour re-chiffrement
        /// des métadonnées. Si None, les métadonnées restent chiffrées
        /// pour l'ancien propriétaire (non recommandé).
        #[serde(skip_serializing_if = "Option::is_none")]
        new_owner_x25519_pubkey: Option<String>,

        /// Métadonnées re-chiffrées pour le nouveau propriétaire (JSON string).
        /// Requis si `new_owner_x25519_pubkey` est présent.
        /// Type String pour éviter dépendance circulaire avec pms-types-payload.
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted_metadata: Option<String>,
    },

    /// Utilisation d'un NFT (action sans destruction).
    ///
    /// Le NFT reste dans le wallet du propriétaire.
    /// Utile pour : tickets, clicker confirmations, achievements, etc.
    Use {
        /// ID du token utilisé
        token_id: String,
        /// Adresse de l'utilisateur (doit être le owner)
        user: String,
        /// Type d'action (ex: "clicker_confirm", "redeem")
        action_type: String,
        /// Données optionnelles associées à l'action
        action_data: Option<String>,
    },

    /// Destruction définitive d'un NFT.
    ///
    /// Le token est retiré de la circulation et ne peut plus être utilisé.
    Burn {
        /// ID du token à détruire
        token_id: String,
        /// Adresse du propriétaire qui brûle (doit signer)
        burner: String,
    },

    /// Destruction définitive de plusieurs NFTs.
    ///
    /// Les tokens sont retirés de la circulation et ne peuvent plus être utilisés.
    BatchBurn {
        /// Liste des IDs des tokens à détruire
        token_ids: Vec<String>,
        /// Adresse du propriétaire qui brûle (doit signer)
        burner: String,
    },
}

impl NftAction {
    /// Retourne le token_id concerné par l'action.
    pub fn token_id(&self) -> &str {
        match self {
            NftAction::Mint { token_id, .. } => token_id,
            NftAction::Transfer { token_id, .. } => token_id,
            NftAction::Use { token_id, .. } => token_id,
            NftAction::Burn { token_id, .. } => token_id,
            NftAction::BatchBurn { token_ids, .. } => {
                if let Some(first) = token_ids.first() {
                    first
                } else {
                    ""
                }
            }
        }
    }

    /// Retourne le type d'action sous forme de chaîne.
    pub fn action_type_str(&self) -> &'static str {
        match self {
            NftAction::Mint { .. } => "mint",
            NftAction::Transfer { .. } => "transfer",
            NftAction::Use { .. } => "use",
            NftAction::Burn { .. } => "burn",
            NftAction::BatchBurn { .. } => "batch_burn",
        }
    }
}
