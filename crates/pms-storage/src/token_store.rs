//! Trait pour la lecture du token registry (assets custom).
//!
//! Abstrait l'accès aux `TokenMetadata` enregistrées via `TokenCreate`,
//! pour que le hot path de validation (`pms-core::net_adapter::persist`)
//! puisse vérifier `mint_authority` / `max_supply` / `decimals` (plan 2.3/2.4)
//! sans dépendre du type concret `RocksStore`.

use anyhow::Result;
use pms_types_payload::{SftClass, TokenMetadata};

/// Lecture du token registry. Implémenté par `RocksStore`
/// (CF `token_registry`, clé = `asset_id`, valeur = JSON `TokenMetadata`).
pub trait TokenRegistryStorage: Send + Sync {
    /// Récupère les métadonnées d'un asset custom, `None` si non enregistré.
    fn get_token(&self, asset_id: &str) -> Result<Option<TokenMetadata>>;
    /// Écrase les métadonnées d'un token **existant** (validées, sans contrôle
    /// d'unicité) — mutations de politique (ex: `RoyaltyUpdate`, protocole 2.7).
    fn put_token(&self, metadata: &TokenMetadata) -> Result<()>;
}

/// Registre des classes semi-fongibles (SFT, `pms-spec-semi-fungibles.md`).
/// Implémenté par `RocksStore` (CF `sft_classes`, clé = `asset_id` =
/// `"collection:class"`, valeur = JSON [`SftClass`]).
pub trait SftClassStorage: Send + Sync {
    /// Enregistre (ou écrase) une classe SFT.
    fn put_sft_class(&self, class: &SftClass) -> Result<()>;
    /// Récupère une classe par son `asset_id`, `None` si non enregistrée.
    fn get_sft_class(&self, asset_id: &str) -> Result<Option<SftClass>>;
    /// Liste toutes les classes (tous collections).
    fn list_sft_classes(&self) -> Result<Vec<SftClass>>;
    /// Liste les classes d'une collection (préfixe `"{collection_id}:"`).
    fn list_sft_classes_by_collection(&self, collection_id: &str) -> Result<Vec<SftClass>>;
}

/// Supertrait « moteur complet » : l'union des capacités de stockage que
/// `CoreAdapter` exige. UNE seule définition de la liste — les impl blocks
/// génériques écrivent `S: EngineStorage` au lieu de recopier (et faire
/// diverger) 7 listes de bounds. Blanket impl : tout type qui satisfait les
/// sous-traits EST un EngineStorage.
pub trait EngineStorage:
    crate::DagStorage
    + crate::NftStorage
    + crate::ConfigStorage
    + crate::GovernanceStorage
    + crate::NodeRewardsStorage
    + crate::ComplianceStorage
    + crate::coordinator_key_store::CoordinatorKeyStorage
    + TokenRegistryStorage
    + SftClassStorage
    + Send
    + Sync
    + 'static
{
}

impl<T> EngineStorage for T where
    T: crate::DagStorage
        + crate::NftStorage
        + crate::ConfigStorage
        + crate::GovernanceStorage
        + crate::NodeRewardsStorage
        + crate::ComplianceStorage
        + crate::coordinator_key_store::CoordinatorKeyStorage
        + TokenRegistryStorage
        + SftClassStorage
        + Send
        + Sync
        + 'static
{
}
