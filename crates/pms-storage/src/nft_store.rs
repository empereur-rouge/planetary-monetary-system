//! Stockage des NFTs (ownership tracking).
//!
//! Ce module définit le trait `NftStorage` et son implémentation
//! pour tracker les propriétaires des NFTs dans la blockchain PMS.
//!
//! ## Modèle de données
//! - Clé : `token_id` (String)
//! - Valeur : `owner_address` (String)
//!
//! ## Voir aussi
//! - Chapitre 10 du Rust Book : Generic Types, Traits, and Lifetimes
//!   https://doc.rust-lang.org/book/ch10-00-generics.html

use anyhow::Result;
use pms_types_nft::NftAction;

/// Trait pour le stockage des NFTs.
///
/// Abstraction permettant différentes implémentations (RocksDB, in-memory, etc.).
///
/// # Exemple
/// ```rust,ignore
/// let owner = nft_store.get_owner("nft-001")?;
/// if owner.is_some() {
///     println!("NFT owned by: {}", owner.unwrap());
/// }
/// ```
pub trait NftStorage: Send + Sync {
    /// Récupère le propriétaire actuel d'un NFT.
    ///
    /// Retourne `None` si le token n'existe pas.
    fn get_owner(&self, token_id: &str) -> Result<Option<String>>;

    /// Définit le propriétaire d'un NFT.
    ///
    /// Utilisé lors du Mint ou du Transfer.
    fn set_owner(&self, token_id: &str, owner: &str) -> Result<()>;

    /// Supprime un NFT (Burn).
    ///
    /// Le token ne sera plus trouvable après cette opération.
    fn delete(&self, token_id: &str) -> Result<()>;

    /// Vérifie si un NFT existe.
    fn exists(&self, token_id: &str) -> Result<bool> {
        Ok(self.get_owner(token_id)?.is_some())
    }

    /// Applique une action NFT au store.
    ///
    /// Cette méthode est appelée après validation pour mettre à jour l'état.
    ///
    /// # Règles
    /// - **Mint** : Crée le NFT avec le creator comme owner
    /// - **Transfer** : Change le owner
    /// - **Use** : Pas de changement d'ownership (optionnel: log usage)
    /// - **Burn** : Supprime le NFT
    fn apply_action(&self, action: &NftAction) -> Result<()> {
        match action {
            NftAction::Mint {
                token_id, creator, ..
            } => self.set_owner(token_id, creator),
            NftAction::Transfer { token_id, to, .. } => self.set_owner(token_id, to),
            NftAction::Use { .. } => {
                // L'action "Use" ne modifie pas l'ownership
                // On pourrait logger l'usage ici si nécessaire
                Ok(())
            }
            NftAction::Burn { token_id, .. } => self.delete(token_id),
        }
    }
}

/// Implémentation in-memory pour les tests.
///
/// Disponible pour les tests d'intégration de tous les crates.
pub mod mock {
    use super::*;
    use std::collections::HashMap;
    use std::sync::RwLock;

    /// Store NFT en mémoire pour les tests unitaires.
    pub struct InMemoryNftStore {
        data: RwLock<HashMap<String, String>>,
    }

    impl InMemoryNftStore {
        pub fn new() -> Self {
            Self {
                data: RwLock::new(HashMap::new()),
            }
        }
    }

    // NOTE: Clippy exige impl Default quand new() n'a pas d'arguments
    impl Default for InMemoryNftStore {
        fn default() -> Self {
            Self::new()
        }
    }

    impl NftStorage for InMemoryNftStore {
        fn get_owner(&self, token_id: &str) -> Result<Option<String>> {
            let data = self.data.read().unwrap();
            Ok(data.get(token_id).cloned())
        }

        fn set_owner(&self, token_id: &str, owner: &str) -> Result<()> {
            let mut data = self.data.write().unwrap();
            data.insert(token_id.to_string(), owner.to_string());
            Ok(())
        }

        fn delete(&self, token_id: &str) -> Result<()> {
            let mut data = self.data.write().unwrap();
            data.remove(token_id);
            Ok(())
        }
    }
}
