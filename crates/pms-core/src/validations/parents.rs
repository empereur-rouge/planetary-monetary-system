use pms_errors::ValidationError;
use pms_types::{Block, PayloadEnvelope, PlainPayload};
use crate::{Dag, ValidatePolicy};

/// Tous les parents référencés existent ?
pub fn parents_exist(dag: &Dag, b: &Block) -> Result<(), ValidationError> {
    for p in &b.parents {
        if !dag.blocks.contains_key(p) {
            return Err(ValidationError::ParentMissing(p.clone()));
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
            return Err(ValidationError::InvalidGenesis("genesis doit être premier et sans parents".into()));
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