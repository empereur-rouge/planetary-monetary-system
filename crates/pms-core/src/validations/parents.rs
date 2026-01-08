use crate::{Dag, ValidatePolicy};
use pms_errors::ValidationError;
use pms_storage::DagStorage;
use pms_types::{Block, PayloadEnvelope, PlainPayload};

// ============================================================================
// VALIDATION DES PARENTS CONTRE LE STORE (RocksDB)
// ============================================================================
//
// ## Pourquoi cette fonction est nécessaire ?
//
// Dans un environnement haute performance avec plusieurs workers parallèles,
// les tips sont sélectionnés depuis RocksDB (`store.top_tips()`), mais la
// validation originale `parents_exist()` vérifiait en RAM (`dag.blocks`).
//
// Race condition typique :
//   1. Worker A récupère tip X depuis RocksDB
//   2. Worker B soumet un bloc, X n'est plus un tip
//   3. Worker A soumet avec parent X
//   4. X existe dans RocksDB mais pas forcément chargé en RAM → REJET
//
// Cette fonction vérifie directement dans le store persistant.
//
// Voir: The Rust Programming Language, Chapitre 16 - Fearless Concurrency
// ============================================================================

/// Vérifie que tous les parents existent dans le store persistant (RocksDB).
///
/// Cette version async est utilisée pour la validation haute performance
/// où les tips viennent du store et non de la RAM.
///
/// ## Arguments
/// * `store` - Le storage persistant (RocksDB)
/// * `b` - Le bloc dont on vérifie les parents
///
/// ## Retour
/// * `Ok(())` si tous les parents existent
/// * `Err(ParentMissing)` si un parent n'existe pas
pub async fn parents_exist_in_store<S>(store: &S, b: &Block) -> Result<(), ValidationError>
where
    S: DagStorage + Send + Sync,
{
    for p in &b.parents {
        // On vérifie dans le store persistant, pas en RAM
        match store.get_block(p).await {
            Ok(Some(_)) => continue, // Parent existe ✓
            Ok(None) => return Err(ValidationError::ParentMissing(p.clone())),
            Err(_) => return Err(ValidationError::ParentMissing(p.clone())),
        }
    }
    Ok(())
}

/// L’ajout de `b` n’introduit pas de cycle ?
pub(crate) fn no_cycle(b: &Block, p: &ValidatePolicy) -> Result<(), ValidationError> {
    if p.forbid_self_parent && b.parents.iter().any(|x| x == &b.id) {
        return Err(ValidationError::SelfParent);
    }
    Ok(())
}

/// Nombre de parents attendu (bootstrapping vs régime normal).
pub fn parent_count(dag: &Dag, b: &Block, policy: &ValidatePolicy) -> Result<(), ValidationError> {
    let n_existing = dag.blocks.len();
    if let Some(PayloadEnvelope::Plain(PlainPayload::Genesis)) = &b.payload {
        if n_existing != 0 || !b.parents.is_empty() {
            return Err(ValidationError::InvalidGenesis(
                "genesis doit être premier et sans parents".into(),
            ));
        }
        return Ok(());
    }

    // Hors genesis :
    if n_existing == 0 {
        return Err(ValidationError::InvalidGenesis("genesis manquant".into()));
    }

    // Bootstrap: autoriser 1 parent tant qu’on n’a pas assez de tips pour en exiger 2+
    let tips = dag.tip_count();
    let target = if n_existing <= 1 {
        1
    } else {
        policy.min_parents_after_boot.min(tips.max(1))
    };

    if b.parents.len() < target {
        return Err(ValidationError::NotEnoughParents);
    }
    Ok(())
}
