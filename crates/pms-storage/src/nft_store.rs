//! Stockage des NFTs (ownership tracking + block_id reference).
//!
//! Ce module définit le trait `NftStorage` et son implémentation
//! pour tracker les propriétaires des NFTs dans la blockchain PMS.
//!
//! ## Modèle de données (Privacy-First)
//! - `token_id` → `owner_address` (ownership)
//! - `token_id` → `block_id` (référence au bloc contenant les métadonnées chiffrées)
//!
//! Les métadonnées ne sont JAMAIS stockées en clair. Elles restent chiffrées
//! dans le bloc du DAG et seuls owner + coordinateur peuvent les déchiffrer.
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

    /// Récupère la liste des token_id appartenant à un propriétaire.
    ///
    /// Retourne un vecteur vide si aucun NFT n'appartient à cette adresse.
    ///
    /// # Performance
    /// Cette méthode peut être coûteuse si le propriétaire possède beaucoup de NFTs.
    /// En production, envisager une pagination.
    fn get_by_owner(&self, owner: &str) -> Result<Vec<String>>;

    /// Récupère l'ID du bloc contenant les métadonnées chiffrées.
    ///
    /// Retourne `None` si le token n'existe pas.
    /// Les métadonnées elles-mêmes sont dans le bloc du DAG (chiffrées).
    fn get_block_id(&self, token_id: &str) -> Result<Option<String>>;

    /// Définit l'ID du bloc contenant les métadonnées chiffrées.
    ///
    /// Utilisé lors du Mint pour référencer le bloc source.
    fn set_block_id(&self, token_id: &str, block_id: &str) -> Result<()>;

    /// Supprime la référence au bloc d'un NFT.
    ///
    /// Appelé lors du Burn pour nettoyer le storage.
    fn delete_block_id(&self, token_id: &str) -> Result<()>;

    /// Vérifie si un NFT existe.
    fn exists(&self, token_id: &str) -> Result<bool> {
        Ok(self.get_owner(token_id)?.is_some())
    }

    /// Applique une action NFT au store.
    ///
    /// Cette méthode est appelée après validation pour mettre à jour l'état.
    /// NOTE: Pour Mint, utilisez `apply_mint` qui accepte le block_id.
    /// NOTE: Pour Transfer avec re-encryption, utilisez `apply_transfer`.
    ///
    /// # Règles
    /// - **Transfer** : Change le owner (pour re-encryption, utilisez apply_transfer)
    /// - **Use** : Pas de changement d'ownership (optionnel: log usage)
    /// - **Burn** : Supprime le NFT et sa référence bloc
    fn apply_action(&self, action: &NftAction) -> Result<()> {
        match action {
            NftAction::Mint { .. } => {
                // Pour Mint, utiliser apply_mint qui accepte le block_id
                anyhow::bail!("Use apply_mint() for Mint actions - block_id is required")
            }
            NftAction::Transfer { token_id, to, .. } => {
                // Transfer simple: change owner, garde block_id
                // Pour re-encryption, utilisez apply_transfer avec le nouveau block_id
                self.set_owner(token_id, to)
            }
            NftAction::Use { .. } => {
                // L'action "Use" ne modifie pas l'ownership
                Ok(())
            }
            NftAction::Burn { token_id, .. } => {
                self.delete_block_id(token_id)?;
                self.delete(token_id)
            }
            NftAction::BatchBurn { token_ids, .. } => {
                for token_id in token_ids {
                    self.delete_block_id(token_id)?;
                    self.delete(token_id)?;
                }
                Ok(())
            }
        }
    }

    /// Applique un Mint avec le block_id source.
    ///
    /// Les métadonnées sont dans le bloc, pas dans le store.
    ///
    /// **Create-only** : un Mint ne réécrit JAMAIS un token existant. Seul le
    /// Mint passe par ici (Transfer utilise `set_owner`/`apply_transfer`), donc
    /// ce garde est la moitié « stockage » de la protection dual-layer contre le
    /// hijack d'ownership par re-mint (l'autre moitié est le pré-check dans le
    /// handler `mint_nft` + `validate_nft_action` au consensus). Cf. revue
    /// sécurité v0.30.1.
    fn apply_mint(&self, token_id: &str, creator: &str, block_id: &str) -> Result<()> {
        if self.get_owner(token_id)?.is_some() {
            anyhow::bail!(
                "NFT {token_id} already exists — mint is create-only (refusing to overwrite owner)"
            );
        }
        self.set_owner(token_id, creator)?;
        self.set_block_id(token_id, block_id)?;
        Ok(())
    }

    /// Applique un Transfer avec mise à jour du block_id (re-encryption).
    ///
    /// Utilisé quand le coordinateur re-chiffre les métadonnées pour le nouveau owner.
    /// Le block_id pointe maintenant vers le bloc Transfer au lieu du bloc Mint.
    fn apply_transfer(&self, token_id: &str, new_owner: &str, new_block_id: &str) -> Result<()> {
        self.set_owner(token_id, new_owner)?;
        self.set_block_id(token_id, new_block_id)?;
        Ok(())
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
        /// Ownership: token_id -> owner_address
        owners: RwLock<HashMap<String, String>>,
        /// Block reference: token_id -> block_id (contient les métadonnées chiffrées)
        block_ids: RwLock<HashMap<String, String>>,
    }

    impl InMemoryNftStore {
        pub fn new() -> Self {
            Self {
                owners: RwLock::new(HashMap::new()),
                block_ids: RwLock::new(HashMap::new()),
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
            let data = self.owners.read().unwrap_or_else(|p| p.into_inner());
            Ok(data.get(token_id).cloned())
        }

        fn set_owner(&self, token_id: &str, owner: &str) -> Result<()> {
            let mut data = self.owners.write().unwrap_or_else(|p| p.into_inner());
            data.insert(token_id.to_string(), owner.to_string());
            Ok(())
        }

        fn delete(&self, token_id: &str) -> Result<()> {
            let mut data = self.owners.write().unwrap_or_else(|p| p.into_inner());
            data.remove(token_id);
            Ok(())
        }

        /// Implémentation simple par scan linéaire.
        /// Acceptable pour les tests, mais inefficace pour de gros volumes.
        fn get_by_owner(&self, owner: &str) -> Result<Vec<String>> {
            let data = self.owners.read().unwrap_or_else(|p| p.into_inner());
            let tokens: Vec<String> = data
                .iter()
                .filter(|(_, v)| *v == owner)
                .map(|(k, _)| k.clone())
                .collect();
            Ok(tokens)
        }

        fn get_block_id(&self, token_id: &str) -> Result<Option<String>> {
            let data = self.block_ids.read().unwrap_or_else(|p| p.into_inner());
            Ok(data.get(token_id).cloned())
        }

        fn set_block_id(&self, token_id: &str, block_id: &str) -> Result<()> {
            let mut data = self.block_ids.write().unwrap_or_else(|p| p.into_inner());
            data.insert(token_id.to_string(), block_id.to_string());
            Ok(())
        }

        fn delete_block_id(&self, token_id: &str) -> Result<()> {
            let mut data = self.block_ids.write().unwrap_or_else(|p| p.into_inner());
            data.remove(token_id);
            Ok(())
        }
    }
}
