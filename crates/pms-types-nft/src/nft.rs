//! Structure principale d'un NFT.
//!
//! Un NFT est identifié de manière unique par son `token_id`.
//! Il appartient à une adresse (`owner`) et peut avoir des métadonnées.

use serde::{Deserialize, Serialize};

/// Représente un NFT (Non-Fungible Token).
///
/// # Champs
/// - `token_id` : Identifiant unique du token (généralement un hash ou UUID)
/// - `owner` : Adresse bech32 du propriétaire actuel
/// - `creator` : Adresse du créateur original (immuable)
/// - `metadata` : Données associées au NFT
///
/// # Exemple
/// ```rust,ignore
/// let nft = Nft {
///     token_id: "nft-abc123".into(),
///     owner: "8e1abc...".into(),
///     creator: "8e1xyz...".into(),
///     metadata: NftMetadata::default(),
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Nft {
    /// Identifiant unique du token (ex: hash, UUID)
    pub token_id: String,

    /// Adresse du propriétaire actuel (format bech32)
    pub owner: String,

    /// Adresse du créateur original (immuable après mint)
    pub creator: String,

    /// Métadonnées du NFT
    pub metadata: NftMetadata,
}

/// Métadonnées associées à un NFT.
///
/// Structure extensible pour stocker les informations du token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct NftMetadata {
    /// Nom du NFT (ex: "Ticket Concert 2024")
    pub name: Option<String>,

    /// Description textuelle
    pub description: Option<String>,

    /// URI vers l'asset (image, fichier, etc.)
    pub uri: Option<String>,

    /// Type de NFT (ex: "ticket", "collectible", "clicker")
    pub nft_type: Option<String>,

    /// Données supplémentaires en JSON libre
    pub extra: Option<String>,
}

impl Nft {
    /// Crée un nouveau NFT avec les informations minimales.
    pub fn new(token_id: String, owner: String, creator: String) -> Self {
        Self {
            token_id,
            owner,
            creator,
            metadata: NftMetadata::default(),
        }
    }

    /// Crée un NFT avec métadonnées complètes.
    pub fn with_metadata(
        token_id: String,
        owner: String,
        creator: String,
        metadata: NftMetadata,
    ) -> Self {
        Self {
            token_id,
            owner,
            creator,
            metadata,
        }
    }
}
