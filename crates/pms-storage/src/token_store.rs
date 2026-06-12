//! Trait pour la lecture du token registry (assets custom).
//!
//! Abstrait l'accès aux `TokenMetadata` enregistrées via `TokenCreate`,
//! pour que le hot path de validation (`pms-core::net_adapter::persist`)
//! puisse vérifier `mint_authority` / `max_supply` / `decimals` (plan 2.3/2.4)
//! sans dépendre du type concret `RocksStore`.

use anyhow::Result;
use pms_types_payload::TokenMetadata;

/// Lecture du token registry. Implémenté par `RocksStore`
/// (CF `token_registry`, clé = `asset_id`, valeur = JSON `TokenMetadata`).
pub trait TokenRegistryStorage: Send + Sync {
    /// Récupère les métadonnées d'un asset custom, `None` si non enregistré.
    fn get_token(&self, asset_id: &str) -> Result<Option<TokenMetadata>>;
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
    + crate::NodeRewardsStorage
    + crate::ComplianceStorage
    + crate::coordinator_key_store::CoordinatorKeyStorage
    + TokenRegistryStorage
    + Send
    + Sync
    + 'static
{
}

impl<T> EngineStorage for T where
    T: crate::DagStorage
        + crate::NftStorage
        + crate::ConfigStorage
        + crate::NodeRewardsStorage
        + crate::ComplianceStorage
        + crate::coordinator_key_store::CoordinatorKeyStorage
        + TokenRegistryStorage
        + Send
        + Sync
        + 'static
{
}
