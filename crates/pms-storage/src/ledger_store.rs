//! Stockage des définitions de ledgers (persistence RocksDB).
//!
//! Ce module définit le trait [`LedgerDefStorage`] pour la persistence
//! des [`LedgerDef`] qui permet aux ledgers créés dynamiquement de survivre
//! aux redémarrages, et de tracer les changements d'ownership.
//!
//! ## Modèle de données
//! - Column family: `ledger_defs`
//! - Clé: `ledger_id` (String)
//! - Valeur: `LedgerDef` (JSON sérialisé)

use anyhow::Result;
use pms_config::LedgerDef;

/// Trait pour la persistence des définitions de ledgers.
///
/// Les ledgers créés dynamiquement via l'API admin sont persistés ici
/// et rechargés au démarrage. Les ledgers du `config.toml` restent
/// la source de vérité pour les ledgers pré-configurés, mais les
/// changements d'ownership sont toujours persistés dans RocksDB.
pub trait LedgerDefStorage: Send + Sync {
    /// Récupère la définition d'un ledger.
    fn get_ledger_def(&self, ledger_id: &str) -> Result<Option<LedgerDef>>;

    /// Persiste ou met à jour une définition de ledger.
    fn put_ledger_def(&self, def: &LedgerDef) -> Result<()>;

    /// Liste toutes les définitions de ledgers persistées.
    fn list_ledger_defs(&self) -> Result<Vec<LedgerDef>>;

    /// Met à jour le `owner_pubkey` d'un ledger.
    ///
    /// Retourne la définition mise à jour. Erreur si le ledger n'est pas trouvé.
    fn update_owner(&self, ledger_id: &str, new_owner: Option<String>) -> Result<LedgerDef>;
}
