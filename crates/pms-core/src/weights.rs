// pms-core/src/dag.rs (ou un module voisin)

use crate::dag::Dag;
use pms_types::BlockId;
use std::collections::{HashMap, VecDeque};

impl Dag {
    /// Calcule le "cumulative weight" façon IOTA.
    /// Algorithme:
    ///   - w[b] = 1 pour tous les blocks
    ///   - on part des tips (no children) et on remonte:
    ///       pour chaque parent p de b: w[p] += w[b]
    ///       décrémente le compteur d'enfants restants du parent; quand il tombe à 0, on enfile le parent
    pub fn cumulative_weights(&self) -> HashMap<BlockId, u64> {
        // 1) Initialisation: poids à 1, et compteur d'enfants (out-degree)
        let mut weight: HashMap<BlockId, u64> =
            self.blocks.keys().map(|id| (id.clone(), 1u64)).collect();

        // out_degree[id] = nombre d'enfants (combien de blocks référencent `id` comme parent)
        let mut out_degree: HashMap<BlockId, u32> =
            self.blocks.keys().map(|id| (id.clone(), 0u32)).collect();

        for (_id, blk) in &self.blocks {
            for parent in &blk.parents {
                if let Some(od) = out_degree.get_mut(parent) {
                    *od += 1;
                }
            }
        }

        // 2) Queue initiale = tips (out_degree == 0)
        let mut q: VecDeque<BlockId> = out_degree
            .iter()
            .filter_map(|(id, &deg)| if deg == 0 { Some(id.clone()) } else { None })
            .collect();

        // 3) Remontée: pour chaque block b, ajoute son poids aux parents, puis "débloque" les parents
        while let Some(bid) = q.pop_front() {
            let w_b = weight[&bid];

            // récupère les parents du block courant
            if let Some(block) = self.blocks.get(&bid) {
                for parent in &block.parents {
                    // w[parent] += w[child]
                    if let Some(wp) = weight.get_mut(parent) {
                        *wp = wp.saturating_add(w_b);
                    }

                    // décrémente le compteur d'enfants restants du parent
                    if let Some(od) = out_degree.get_mut(parent) {
                        *od -= 1;
                        if *od == 0 {
                            q.push_back(parent.clone());
                        }
                    }
                }
            }
        }

        weight
    }
}
