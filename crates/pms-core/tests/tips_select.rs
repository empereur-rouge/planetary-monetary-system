use anyhow::Result;
use async_trait::async_trait;
use pms_core::Dag;
use pms_core::tips::select_parents_deterministic;
use pms_storage::store::PutResult;
use pms_storage::{DagStorage, LedgerMutation, StoredBlock, UtxoDelta};
use pms_types::Block;
use pms_utils::compute_block_id;
use pms_wire::WireBlock;

// ---- Dummy store: ne fournit que top_tips() pour ce test.
struct DummyStore {
    tips: Vec<String>,
}

#[async_trait]
impl DagStorage for DummyStore {
    async fn put_block(&self, _b: &StoredBlock) -> Result<PutResult> {
        Ok(PutResult::Inserted)
    }
    async fn get_block(&self, _id: &str) -> Result<Option<StoredBlock>> {
        Ok(None)
    }
    async fn add_child_edge(&self, _parent: &str, _child: &str) -> Result<()> {
        Ok(())
    }
    async fn children_count(&self, _id: &str) -> Result<u64> {
        Ok(0)
    }
    async fn add_tip(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    async fn remove_tip(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    async fn top_tips(&self, _limit: usize) -> Result<Vec<String>> {
        Ok(self.tips.clone())
    }
    async fn all_block_ids(&self) -> Result<Vec<String>> {
        Ok(vec![])
    }
    async fn export_json(&self) -> Result<String> {
        Ok("[]".into())
    }

    async fn export_namespace(&self) -> Result<String> {
        todo!()
    }

    async fn import_json(&self, _dump: &str) -> Result<()> {
        Ok(())
    }

    async fn append_block_atomic(&self, _b: &StoredBlock) -> Result<bool> {
        todo!()
    }

    async fn append_block_atomic_with_utxo(
        &self,
        _b: &StoredBlock,
        _delta: Option<&UtxoDelta>,
    ) -> Result<bool> {
        todo!()
    }

    async fn load_final(&self) -> Result<Vec<String>> {
        todo!()
    }

    async fn load_last_milestone(&self) -> Result<Option<String>> {
        todo!()
    }

    async fn recent_ids(&self, limit: usize) -> Result<Vec<String>> {
        todo!()
    }

    async fn recent_ids_by_time(
        &self,
        after_ts: Option<i64>,
        after_id: Option<String>,
        limit: usize,
    ) -> Result<(Vec<String>, Option<(i64, String, bool)>)> {
        todo!()
    }

    async fn get_blocks_by_ids(&self, ids: &[String]) -> Result<Vec<WireBlock>> {
        todo!()
    }

    async fn persist_final(&self, ids: &[String]) -> Result<()> {
        todo!()
    }

    async fn persist_last_milestone(&self, id: &str) -> Result<()> {
        todo!()
    }
    // (si votre trait inclut append_block_atomic/persist_genesis, fournissez des stubs)
}

// ---- Construit un DAG RAM avec genesis + blocs "a".."g" (parents=genesis), et marque `finalized` fournis.
fn mk_dag_with_seed(seed: Option<&str>, finalized: &[&str]) -> Dag {
    let genesis = Block::genesis(compute_block_id);
    let mut dag = Dag::new_with_genesis(genesis);

    // index rapide pour marquer finalisés
    let finalized_set: std::collections::HashSet<String> =
        finalized.iter().map(|s| s.to_string()).collect();

    // retrouve l’id du genesis
    let genesis_id = dag
        .blocks
        .iter()
        .find(|(_, b)| b.parents.is_empty())
        .map(|(id, _)| id.clone())
        .unwrap();

    // insère en RAM des blocs a..g comme tips (parents = [genesis])
    for id in ["a", "b", "c", "d", "e", "f", "g"] {
        let b = Block {
            id: id.to_string(),
            parents: vec![genesis_id.clone()],
            payload: None,
            nonce: 0,
            metadata: None,
            signer_pk: None,
            signature: None,
        };
        dag.blocks.insert(id.to_string(), b);
        *dag.children.entry(genesis_id.clone()).or_default() += 1;
    }

    // seed = last_milestone
    if let Some(s) = seed {
        dag.finality.last_milestone = Some(s.to_string());
    }

    // marque finalisée dans la structure de finalité (impl concrète selon votre type)
    for id in finalized_set {
        dag.finality.finalized.insert(id);
    }

    dag
}

#[tokio::test]
async fn tips_selection_is_deterministic_and_excludes_finalized() -> Result<()> {
    // Store: tips disponibles (ordre arbitraire)
    let store = DummyStore {
        tips: vec![
            "a".into(),
            "b".into(),
            "c".into(),
            "d".into(),
            "e".into(),
            "f".into(),
            "g".into(),
        ],
    };

    // DAG RAM : même ensemble a..g présent, seed=ms1, et "c" est finalisé (doit être exclu)
    let dag = mk_dag_with_seed(Some("ms1"), &["c"]);

    // 1) Sélection k=3, fenêtre large
    let k = 3usize;
    let sel1 = select_parents_deterministic(&dag, &store, k, 256).await?;
    assert_eq!(sel1.len(), k, "doit retourner exactement k parents");
    assert!(
        !sel1.iter().any(|x| x == "c"),
        "ne doit pas contenir un bloc finalisé"
    );

    // 2) Déterminisme (même seed, même entrée → même résultat)
    let sel2 = select_parents_deterministic(&dag, &store, k, 256).await?;
    assert_eq!(sel1, sel2, "même seed => même ordre de parents");

    // 3) Seed différent => ordre très probablement différent
    let dag2 = mk_dag_with_seed(Some("ms2"), &["c"]);
    let sel3 = select_parents_deterministic(&dag2, &store, k, 256).await?;
    assert_ne!(
        sel1, sel3,
        "seed différent => ordre (très probablement) différent"
    );

    Ok(())
}
