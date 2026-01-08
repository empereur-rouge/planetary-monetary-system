//! Trait pour le stockage des récompenses de nœuds.
//!
//! Ce module gère le pool de fees destinés aux nœuds et le comptage
//! des blocs minés par chaque nœud pour la distribution proportionnelle.

use anyhow::Result;

/// Trait abstrayant le stockage des récompenses de nœuds.
///
/// Implémenté par RocksStore pour la persistance du pool de fees
/// et des compteurs de blocs par nœud.
pub trait NodeRewardsStorage: Send + Sync {
    /// Récupère le nombre de blocs minés par un nœud.
    fn get_node_block_count(&self, node_pk: &str) -> Result<u64>;

    /// Incrémente le compteur de blocs pour un nœud.
    fn increment_node_block_count(&self, node_pk: &str) -> Result<()>;

    /// Récupère le montant total dans le pool de fees (en satoshis/unité atomique).
    fn get_fee_pool(&self) -> Result<u64>;

    /// Ajoute un montant au pool de fees.
    fn add_to_fee_pool(&self, amount: u64) -> Result<()>;

    /// Récupère tous les mineurs et leurs compteurs de blocs.
    ///
    /// Retourne une liste de (public_key, block_count).
    fn get_all_miners(&self) -> Result<Vec<(String, u64)>>;

    /// Réinitialise le pool et tous les compteurs après distribution.
    fn reset_pool_and_counts(&self) -> Result<()>;

    /// Définit l'adresse de récompense pour un nœud.
    fn set_node_reward_address(&self, node_pk: &str, address: &str) -> Result<()>;

    /// Récupère l'adresse de récompense d'un nœud.
    /// Par défaut, retourne la clé publique elle-même si aucune adresse n'est définie.
    fn get_node_reward_address(&self, node_pk: &str) -> Result<String>;
}
