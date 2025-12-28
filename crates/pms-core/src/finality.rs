use std::collections::{HashSet, VecDeque};
use crate::Dag;

/// État finalité minimal : on garde l’id du dernier milestone + l’ensemble des blocs finalisés.
#[derive(Default)]
pub struct FinalityState {
    /// blocs finalisés (ids)
    pub finalized: HashSet<String>,
    /// dernier milestone (seed pour tips deterministes)
    pub last_milestone: Option<String>,
    /// seuil de confirmations (profondeur) pour finaliser automatiquement
    pub depth_k: usize,
}


impl FinalityState {
    pub fn new(depth_k: usize) -> Self {
        Self { finalized: HashSet::new(), last_milestone: None, depth_k }
    }

    pub fn is_final(&self, id: &str) -> bool {
        self.finalized.contains(id)
    }

    pub fn mark_final(&mut self, id: &str) {
        self.finalized.insert(id.to_string());
    }

    pub fn set_milestone(&mut self, id: String) {
        self.last_milestone = Some(id.clone());
        self.mark_final(&id);
    }
}

/// Retourne vrai si `b` a au moins `k` confirmations (descendants distincts).
pub fn has_k_confirmations_dag(dag: &Dag, b: &str, k: usize) -> bool {
    if k == 0 {
        return true;
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut q = VecDeque::new();

    // 1) Seed: tous les blocs dont `b` est un parent direct
    for (id, blk) in dag.blocks.iter() {
        if blk.parents.iter().any(|p| p == b) {
            if seen.insert(id.clone()) {
                q.push_back(id.clone());
            }
        }
    }

    // 2) BFS descendants en suivant les parents
    while let Some(x) = q.pop_front() {
        if seen.len() >= k {
            return true;
        }

        // enfants de x = tous les blocs qui ont x dans leurs parents
        for (id, blk) in dag.blocks.iter() {
            if blk.parents.iter().any(|p| p == &x) {
                if seen.insert(id.clone()) {
                    q.push_back(id.clone());
                }
            }
        }
    }

    seen.len() >= k
}