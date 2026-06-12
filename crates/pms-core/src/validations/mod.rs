pub mod amount;
pub mod apply;
pub mod authority; // Autorité coordinator-only par type de payload (audit C-2 extension)
pub mod check;
pub mod conditions; // Spend conditions des outputs : MultiSig / HashLock (protocole 2.2)
pub mod fees;
mod impls;
pub mod mint; // Validation de sécurité du Minting (Coordinateur Only)
pub mod nft; // Validation des actions NFT
pub mod ownership; // Binding unlock pubkey ↔ propriétaire UTXO (audit C-1)
pub mod parents; // Public pour exposer parents_exist_in_store
pub mod policy;
pub mod signature; // Public : vérification des unlocks réutilisée par les handlers (audit C-2)
mod traits;
pub mod transactions;
