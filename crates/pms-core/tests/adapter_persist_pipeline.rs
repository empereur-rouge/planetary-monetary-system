use std::sync::Arc;

use anyhow::Result;
use tempfile::tempdir;
use tokio::sync::Mutex;

use pms_config::load_config;
use pms_storage::rocks_store::store::RocksStore;
use pms_wire::{WireBlock, WireMeta};
use pms_wallet::{SignerBackend, Wallet};
use pms_core::{Dag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::{DagStorage, PutResult};
use pms_testkit::forge_signed_wire_block_for_test;
// ← adapte le chemin selon ton crate // idem, si c’est dans un autre module

// Petit alias local, comme dans le reste de ton code.
type DagRef = Arc<Mutex<Dag>>;

/// Test d’intégration “happy path” :
///
/// 1. Crée une RocksDB temporaire + DAG.
/// 2. Forge un WireBlock signé et cohérent (network_id / proto / parents).
/// 3. Appelle `adapter.persist_block(&wb)`.
/// 4. Vérifie :
///    - que le bloc est bien présent en store,
///    - qu’il est attaché dans le DAG en RAM,
///    - que la finalité a été mise à jour sans panique.
#[tokio::test]
async fn signed_block_goes_through_full_pipeline() -> Result<()> {
    // ------------------------------------------------------------
    // 1) RocksDB éphémère + store
    // ------------------------------------------------------------
    let dir = tempdir()?;
    let db_path = dir.path().join("rocks-core-pipeline");

    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256,          // tip_limit pour les tests
            "pms:test",   // prefix de test
        )
            .await?,
    );

    // ------------------------------------------------------------
    // 2) Config réseau + meta (network_id / protocol_version)
    // ------------------------------------------------------------
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // ------------------------------------------------------------
    // 3) DAG : bootstrap depuis le store (création du genesis si vide)
    // ------------------------------------------------------------
    let dag_loaded = Dag::bootstrap_from_store_or_new_dag(&*store, &meta).await?;
    let dag: DagRef = Arc::new(Mutex::new(dag_loaded));

    // ------------------------------------------------------------
    // 4) Adapter (CoreAdapter) avec policy par défaut
    //    → c’est lui qui contient persist_block() + validate_wire_block_pow()
    // ------------------------------------------------------------
    let adapter = CoreAdapter::new(dag.clone(), store.clone());

    // ------------------------------------------------------------
    // 5) Choix des parents : tips actuels ou fallback sur le genesis
    // ------------------------------------------------------------
    let mut parents = store.top_tips(2).await?;
    if parents.is_empty() {
        let all = store.all_block_ids().await?;
        if let Some(first) = all.first() {
            parents.push(first.clone());
        }
    }
    parents.sort();
    parents.dedup();

    // ------------------------------------------------------------
    // 6) Wallet de test (clé ECDSA) pour signer le bloc
    // ------------------------------------------------------------
    let wallet = Wallet::from_seed(&[3u8; 32], None)
        .expect("Wallet::from_seed ne doit pas fail en test");

    // ------------------------------------------------------------
    // 7) Forge d’un WireBlock signé et cohérent
    //
    //    - id vide dans un premier temps,
    //    - network_id / protocol_version depuis `meta`,
    //    - signer_pk_hex = clé publique du wallet,
    //    - signature calculée sur le message canonique,
    //    - id calculé ensuite avec compute_block_id().
    // ------------------------------------------------------------
    let payload = None; // MVP: bloc sans payload

    let mut wb = forge_signed_wire_block_for_test(
        parents.clone(),
        &meta,
        &wallet,
        1,          // nonce arbitraire pour le test
        payload,
    );

    // ------------------------------------------------------------
    // 8) Appel de la pipeline centrale : persist_block(&wb)
    //
    //    Ce call doit:
    //      - valider network_id / proto / signature / PoW,
    //      - appliquer la policy (taille payload, parents, etc.),
    //      - valider le DAG via validate_block(),
    //      - persister en RocksDB,
    //      - mettre à jour le DAG et la finalité.
    // ------------------------------------------------------------
    let res = adapter.persist_block(&wb).await?;
    assert!(
        matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
        "bloc signé devrait être accepté par la pipeline, obtenu: {res:?}"
    );

    // ------------------------------------------------------------
    // 9) Vérif store : le bloc doit exister en RocksDB
    // ------------------------------------------------------------
    let stored = store.get_block(&wb.id).await?;
    assert!(
        stored.is_some(),
        "le store doit contenir le bloc après persist_block()"
    );

    // ------------------------------------------------------------
    // 10) Vérif DAG : bloc attaché + finalité cohérente
    // ------------------------------------------------------------
    {
        let dag_guard = dag.lock().await;

        // Présence dans la map des blocs
        assert!(
            dag_guard.blocks.contains_key(&wb.id),
            "le DAG en RAM doit contenir le bloc inséré"
        );

        // Finalité : on ne teste pas une valeur précise, mais on vérifie que
        // l’accès à la structure ne panique pas et que la collection est cohérente.
        // Tu peux affiner si tu as une règle précise (ex: genesis toujours final).
        let _finalized: Vec<String> = dag_guard.finality.finalized.iter().cloned().collect();
        // eprintln!("[TEST] finalized={:?}", _finalized);
    }

    Ok(())
}