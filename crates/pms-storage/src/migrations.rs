//! pms-stockage/src/migrations.rs
//!
//! Mécanisme **simple** de migrations de schéma Redis pour le DAG.
//! - Version stockée sous la clé `pms:ver` (entier croissant).
//! - `ensure_schema()` lit la version et applique en chaîne les migrations manquantes.
//! - Chaque migration est **idempotente** (sûre à relancer).
//!
//! Bonnes pratiques :
//! - Incrémentez `CURRENT_VER` quand vous ajoutez une nouvelle étape.
//! - Ajoutez une fonction `mig_<n>_to_<n+1>()` et référencez‑la dans `ensure_schema()`.
//! - Visez des opérations atomiques/commutatives côté Redis (SADD/ZADD/HSET…), ou
//!   structurez votre code pour supporter les relances (idempotence).

use thiserror::Error;

/// Version logique du schéma attendu par ce binaire.
/// Incrémentez lorsqu'une nouvelle migration est introduite.
pub const CURRENT_VER: i64 = 11;

/// Version du protocole DAG (SemVer).
/// - MAJOR : changement incompatible (refus de démarrer, migration manuelle requise)
/// - MINOR : nouvelles fonctionnalités backward-compatible (migration auto)
/// - PATCH : correctifs (migration auto)
/// v3.0.0 (audit sécurité 2026-06-11) : règles de validation BREAKING —
/// les unlocks de transaction sont vérifiés (signature + ownership,
/// appariement input[i]↔unlock[i]) et le block id doit être le hash
/// canonique du contenu. Des blocs acceptés sous 2.x (unlocks invalides,
/// ids forgés) sont rejetés sous 3.x ; un re-sync depuis zéro peut refuser
/// un historique 2.x → wipe testnet requis.
/// v3.4.0 (plan §4 gouvernance) : nouvelles variantes `PlainPayload::Governance{Proposal,Enact,Cancel}`
/// (changements de config timelockés + ancrés DAG). Additif backward-compatible →
/// migration auto, pas de wipe.
/// v3.3.0 (plan §3.1 voie B) : nouvelle variante `PlainPayload::TokenBurn`
/// (burn de token owner-signé, la supply baisse, trigger des contrats
/// `OnTokenBurn`). Backward-compatible : les blocs existants parsent toujours ;
/// un nœud à jour accepte le nouveau type. MINOR → migration auto, pas de wipe.
pub const DAG_VERSION: &str = "3.4.0";

/// Erreurs possibles lors des migrations.
#[derive(Error, Debug)]
pub enum MigError {
    /// Une version inconnue a été rencontrée (binaire trop ancien / données plus récentes).
    #[error("unexpected version {0}")]
    Unexpected(i64),
    /// Wrapper générique pour erreurs Anyhow.
    #[error(transparent)]
    Any(#[from] anyhow::Error),
}
