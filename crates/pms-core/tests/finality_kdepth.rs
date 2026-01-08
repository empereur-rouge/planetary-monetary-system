// pms-core/tests/finality_kdepth.rs
use anyhow::Result;
use pms_core::Dag;
use pms_types::Block;
use pms_utils::compute_block_id;

#[test]
fn finality_by_k_depth() -> Result<()> {
    // 1) DAG + genesis
    let mut dag = Dag::new_with_genesis(Block::genesis(compute_block_id));

    // règle de finalité: profondeur K
    let k = 5usize;
    dag.finality.depth_k = k;

    // garde l'id du genesis
    let genesis = dag
        .blocks
        .values()
        .find(|b| b.parents.is_empty())
        .expect("genesis présent")
        .id
        .clone();

    // 2) Ajoute >K blocs via l’API high-level (sélection de parents + validations internes)
    let mut created = Vec::new();
    for _ in 0..(k + 3) {
        let b = dag
            .add_payload_auto_parents_mined(None, 0, compute_block_id)
            .expect("add block");
        created.push(b.id.clone());
    }

    // 3) Le premier bloc non-genesis doit devenir final une fois profondeur > K
    //    (on prend le tout premier que nous avons créé)
    let first_non_genesis = created[0].clone();

    assert!(
        dag.is_final(&first_non_genesis),
        "le bloc ancien (profondeur > K) doit être final"
    );

    // Sanity: le genesis doit être final aussi dans cette politique
    assert!(dag.is_final(&genesis), "genesis devrait être final");

    Ok(())
}
