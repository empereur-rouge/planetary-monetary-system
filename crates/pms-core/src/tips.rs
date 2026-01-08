use crate::{Dag, MAX_TIPS_CAP, TIP_CHILDREN_THRESHOLD};
use pms_storage::DagStorage;
use pms_types::BlockId;
use sha2::{Digest, Sha256};

pub async fn select_parents_deterministic<S: DagStorage + Send + Sync>(
    dag: &Dag,
    store: &S,
    k: usize,
    window: usize,
) -> anyhow::Result<Vec<String>> {
    // 0) genesis id RAM (si présent)
    let genesis_id = dag
        .blocks
        .iter()
        .find(|(_, b)| b.parents.is_empty())
        .map(|(id, _)| id.clone());

    // 1) seed = last milestone (ou "genesis")
    let seed = dag.finality.last_milestone.as_deref().unwrap_or("genesis");

    // 2) récup tips récents depuis le store
    let mut tips = store.top_tips(window).await?;

    // 3) filtre store → RAM (évite parents fantômes) + évite finalisés
    tips.retain(|id| dag.blocks.contains_key(id) && !dag.is_final(id));

    // 4) tri déterministe (sha256(seed||tip_id))
    tips.sort_by(|a, b| {
        let mut ha = Sha256::new();
        ha.update(seed.as_bytes());
        ha.update(a.as_bytes());
        let mut hb = Sha256::new();
        hb.update(seed.as_bytes());
        hb.update(b.as_bytes());
        ha.finalize().as_slice().cmp(hb.finalize().as_slice())
    });

    // 5) dédup + tronque à k
    tips.dedup();
    tips.truncate(k);

    // 6) bornes minimales :
    //    - au boot (≤1 bloc en RAM), 1 parent (le genesis)
    //    - sinon, au moins 2 parents (si k >= 2)
    let n_existing = dag.blocks.len();
    let min_needed = if n_existing <= 1 {
        1
    } else {
        core::cmp::min(2, k)
    };

    // 7) fallback: si liste insuffisante, rajoute genesis si dispo
    if tips.len() < min_needed {
        if let Some(gid) = genesis_id {
            if !tips.contains(&gid) {
                tips.push(gid);
            }
        }
    }

    // 8) si encore vide (cas extrême), retourne au moins quelque chose
    if tips.is_empty() {
        // mieux vaut 1 parent que rien (évite "Aucun parent disponible")
        if let Some((gid, _)) = dag.blocks.iter().find(|(_, b)| b.parents.is_empty()) {
            tips.push(gid.clone());
        }
    }

    Ok(tips)
}

impl Dag {
    pub fn tip_count(&self) -> usize {
        self.blocks
            .keys()
            .filter(|id| self.children.get(*id).copied().unwrap_or(0) == 0)
            .count()
    }

    /// Tips = blocks dont children_count < seuil
    pub fn find_tips(&self) -> Vec<BlockId> {
        // 1) candidats
        let mut tips: Vec<BlockId> = self
            .blocks
            .iter()
            .filter(|(id, _)| self.children.get(*id).copied().unwrap_or(0) < TIP_CHILDREN_THRESHOLD)
            .map(|(id, _)| id.clone())
            .collect();

        // 2) si peu de tips, on renvoie tout
        if tips.len() <= MAX_TIPS_CAP {
            return tips;
        }

        // 3) sinon on garde les meilleurs (poids cumulés) ou on échantillonne
        let cw = self.cumulative_weights();

        // — Option A: top‑K par poids
        tips.sort_by(|a, b| {
            let wa = *cw.get(a).unwrap_or(&1);
            let wb = *cw.get(b).unwrap_or(&1);
            wb.cmp(&wa) // desc
        });
        tips.truncate(MAX_TIPS_CAP);
        tips
    }
}
