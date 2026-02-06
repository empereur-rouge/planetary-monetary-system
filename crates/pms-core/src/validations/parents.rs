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

// ============================================================================
// SINGLE WRITER: CHAÎNE LINÉAIRE (1 parent)
// ============================================================================
//
// En mode Single Writer (enforce_single_writer = true), on garantit une chaîne
// strictement linéaire: chaque bloc a exactement 1 parent (sauf genesis qui en a 0).
//
// Avantages:
// - Ordre total déterministe (pas de conflit, pas d'orphelins)
// - Finalité immédiate (pas besoin de k-depth)
// - Performance maximale (pas de résolution de conflits)
//
// ============================================================================

/// Vérifie que le bloc respecte la chaîne linéaire (1 seul parent).
///
/// En mode Single Writer, on refuse tout bloc ayant plus d'un parent.
/// Cela garantit une structure de chaîne simple au lieu d'un DAG.
///
/// ## Arguments
/// * `b` - Le bloc à valider
/// * `is_genesis` - true si le bloc est le genesis (autorise 0 parents)
///
/// ## Retour
/// * `Ok(())` si le bloc a 0 parent (genesis) ou exactement 1 parent
/// * `Err(TooManyParents)` si plus d'un parent
pub fn enforce_single_parent(b: &Block, is_genesis: bool) -> Result<(), ValidationError> {
    if is_genesis {
        // Genesis: doit avoir 0 parents
        if !b.parents.is_empty() {
            return Err(ValidationError::InvalidGenesis(
                "genesis ne doit pas avoir de parents".into(),
            ));
        }
    } else {
        // Non-genesis: doit avoir exactement 1 parent
        if b.parents.len() != 1 {
            return Err(ValidationError::TooManyParents(format!(
                "single_writer: bloc doit avoir exactement 1 parent, got {}",
                b.parents.len()
            )));
        }
    }
    Ok(())
}
