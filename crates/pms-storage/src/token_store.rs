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
