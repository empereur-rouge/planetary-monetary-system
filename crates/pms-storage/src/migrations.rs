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

use anyhow::Result;
use thiserror::Error;

/// Version logique du schéma attendu par ce binaire.
/// Incrémentez lorsqu’une nouvelle migration est introduite.
pub const CURRENT_VER: i64 = 2;

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