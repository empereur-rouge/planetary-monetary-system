//! Implémentation RocksDB de NftStorage.
//!
//! ## Schéma des Column Families (CF):
//! - `nft_ownership`: `token_id` -> `owner_address`
//! - `nfts_by_owner`: `owner_address` -> JSON array de `token_id`s
//! - `nft_block_ids`: `token_id` -> `block_id` (référence au bloc avec métadonnées chiffrées)
//!
//! ## Privacy
//! Les métadonnées ne sont JAMAIS stockées en clair. Seul le `block_id` est stocké
//! pour référencer le bloc du DAG contenant les métadonnées chiffrées.
//!
//! ## Voir aussi
//! - Chapitre 15 du Rust Book : Smart Pointers
//!   https://doc.rust-lang.org/book/ch15-00-smart-pointers.html

use crate::{NftStorage, rocks_store::store::RocksStore};
use anyhow::Result;

impl NftStorage for RocksStore {
    /// Récupère le propriétaire d'un NFT par son token_id.
    fn get_owner(&self, token_id: &str) -> Result<Option<String>> {
        let cf_nft = self.cf("nft_ownership");
        if let Some(v) = self.db.get_cf(cf_nft, token_id.as_bytes())? {
            Ok(Some(String::from_utf8(v.to_vec())?))
        } else {
            Ok(None)
        }
    }

    /// Définit le propriétaire d'un NFT.
    ///
    /// Cette méthode effectue un "dual-write" :
    /// 1. Met à jour `nft_ownership` (token_id -> owner)
    /// 2. Met à jour `nfts_by_owner` (owner -> liste de tokens)
    ///
    /// Si le token avait déjà un propriétaire différent (Transfer),
    /// on le retire de l'ancienne liste avant de l'ajouter à la nouvelle.
    fn set_owner(&self, token_id: &str, new_owner: &str) -> Result<()> {
        let cf_ownership = self.cf("nft_ownership");

        // 1. Récupérer l'ancien propriétaire (si le token existe déjà)
        let old_owner = self.get_owner(token_id)?;

        // 2. Mettre à jour nft_ownership (token -> new_owner)
        self.db
            .put_cf(cf_ownership, token_id.as_bytes(), new_owner.as_bytes())?;

        // 3. Si l'ancien propriétaire est différent, le retirer de sa liste
        if let Some(ref prev) = old_owner {
            if prev != new_owner {
                self.remove_token_from_owner_list(prev, token_id)?;
            }
        }

        // 4. Ajouter le token à la liste du nouveau propriétaire
        //    (sauf si c'est le même que l'ancien, dans ce cas on ne fait rien)
        if old_owner.as_deref() != Some(new_owner) {
            self.add_token_to_owner_list(new_owner, token_id)?;
        }

        Ok(())
    }

    /// Supprime un NFT (Burn).
    ///
    /// Retire le token de `nft_ownership` ET de la liste `nfts_by_owner` du propriétaire.
    fn delete(&self, token_id: &str) -> Result<()> {
        let cf_ownership = self.cf("nft_ownership");

        // 1. Récupérer le propriétaire actuel pour nettoyer la liste inverse
        if let Some(owner) = self.get_owner(token_id)? {
            self.remove_token_from_owner_list(&owner, token_id)?;
        }

        // 2. Supprimer l'entrée principale
        self.db.delete_cf(cf_ownership, token_id.as_bytes())?;

        Ok(())
    }

    /// Récupère tous les token_ids appartenant à un propriétaire.
    ///
    /// Retourne un vecteur vide si l'adresse ne possède aucun NFT.
    fn get_by_owner(&self, owner: &str) -> Result<Vec<String>> {
        let cf_by_owner = self.cf("nfts_by_owner");

        if let Some(v) = self.db.get_cf(cf_by_owner, owner.as_bytes())? {
            // La valeur est un JSON array: ["token1", "token2", ...]
            let tokens: Vec<String> = serde_json::from_slice(&v)?;
            Ok(tokens)
        } else {
            Ok(Vec::new())
        }
    }

    /// Récupère l'ID du bloc contenant les métadonnées chiffrées.
    ///
    /// Retourne `None` si le token n'existe pas.
    fn get_block_id(&self, token_id: &str) -> Result<Option<String>> {
        let cf_block_ids = self.cf("nft_block_ids");
        if let Some(v) = self.db.get_cf(cf_block_ids, token_id.as_bytes())? {
            Ok(Some(String::from_utf8(v.to_vec())?))
        } else {
            Ok(None)
        }
    }

    /// Définit l'ID du bloc contenant les métadonnées chiffrées.
    ///
    /// Appelé lors du Mint pour référencer le bloc source.
    fn set_block_id(&self, token_id: &str, block_id: &str) -> Result<()> {
        let cf_block_ids = self.cf("nft_block_ids");
        self.db
            .put_cf(cf_block_ids, token_id.as_bytes(), block_id.as_bytes())?;
        Ok(())
    }

    /// Supprime la référence au bloc d'un NFT (lors du Burn).
    fn delete_block_id(&self, token_id: &str) -> Result<()> {
        let cf_block_ids = self.cf("nft_block_ids");
        self.db.delete_cf(cf_block_ids, token_id.as_bytes())?;
        Ok(())
    }
}

// === Méthodes privées helpers pour la gestion de la liste inverse ===

impl RocksStore {
    /// Ajoute un token_id à la liste d'un propriétaire dans nfts_by_owner.
    fn add_token_to_owner_list(&self, owner: &str, token_id: &str) -> Result<()> {
        let cf_by_owner = self.cf("nfts_by_owner");

        // Récupérer la liste actuelle (ou créer une liste vide)
        let mut tokens: Vec<String> =
            if let Some(v) = self.db.get_cf(cf_by_owner, owner.as_bytes())? {
                serde_json::from_slice(&v)?
            } else {
                Vec::new()
            };

        // Éviter les doublons (normalement impossible, mais défensif)
        if !tokens.contains(&token_id.to_string()) {
            tokens.push(token_id.to_string());
        }

        // Sérialiser et sauvegarder
        let json = serde_json::to_vec(&tokens)?;
        self.db.put_cf(cf_by_owner, owner.as_bytes(), &json)?;

        Ok(())
    }

    /// Retire un token_id de la liste d'un propriétaire dans nfts_by_owner.
    fn remove_token_from_owner_list(&self, owner: &str, token_id: &str) -> Result<()> {
        let cf_by_owner = self.cf("nfts_by_owner");

        if let Some(v) = self.db.get_cf(cf_by_owner, owner.as_bytes())? {
            let mut tokens: Vec<String> = serde_json::from_slice(&v)?;

            // Retirer le token de la liste
            tokens.retain(|t| t != token_id);

            if tokens.is_empty() {
                // Si la liste est vide, supprimer l'entrée complètement
                self.db.delete_cf(cf_by_owner, owner.as_bytes())?;
            } else {
                // Sinon, sauvegarder la liste mise à jour
                let json = serde_json::to_vec(&tokens)?;
                self.db.put_cf(cf_by_owner, owner.as_bytes(), &json)?;
            }
        }

        Ok(())
    }
}
