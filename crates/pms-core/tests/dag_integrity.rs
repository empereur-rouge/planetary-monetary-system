//! Invariants d'intégrité du DAG (audit Section 4) — red-first.
//!
//! Chaque test soumet un bloc malformé au VRAI chemin de persistance
//! (`CoreAdapter::persist_block`) et vérifie le verdict. Verrouille contre la
//! régression les protections du hot path :
//!   - id == hash canonique du contenu (M-6) ;
//!   - idempotence : re-soumission ⇒ AlreadyExists, pas de double-apply ;
//!   - parent inconnu : jamais d'acceptation silencieuse.

use std::sync::Arc;

use pms_config::load_config;
use pms_core::validations::check::ValidatePolicy;
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::PutResult;
use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use pms_types::{Block, PayloadEnvelope, PlainPayload, TxOutput};
use pms_utils::compute_block_id;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::{WireBlock, WireMeta};

type DagRef = Arc<ConcurrentDag>;

/// Store Rocks éphémère partagé par les helpers (laissé vivant via `forget`).
async fn make_store(tag: &str) -> anyhow::Result<Arc<RocksStore>> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join(format!("rocks-{tag}"));
    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256,
            "pms:test",
            None,
            &RocksMemoryConfig::default(),
        )
        .await?,
    );
    std::mem::forget(dir);
    Ok(store)
}

/// Store Rocks éphémère + DAG genesis + adapter réel (policy dev par défaut :
/// `enforce_parent_existence = false`).
async fn fresh(tag: &str) -> anyhow::Result<(DagRef, Arc<dyn NetDagAdapter>, WireMeta)> {
    let store = make_store(tag).await?;
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(Block::genesis(compute_block_id)));
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store, 0, None);
    Ok((dag, adapter, meta))
}

/// Comme [`fresh`] mais avec `enforce_parent_existence = true` (réglage PROD —
/// config.prod.toml). Permet de tester la protection « parent inconnu rejeté »
/// que la config dev désactive volontairement (bootstrap/tests).
async fn fresh_strict_parents(
    tag: &str,
) -> anyhow::Result<(DagRef, Arc<dyn NetDagAdapter>, WireMeta)> {
    let store = make_store(tag).await?;
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(Block::genesis(compute_block_id)));
    let mut policy = ValidatePolicy::from_global_config();
    policy.enforce_parent_existence = true; // ← réglage prod
    let adapter: Arc<dyn NetDagAdapter> =
        CoreAdapter::new_with_policy(dag.clone(), store, policy, 0, None);
    Ok((dag, adapter, meta))
}

/// Forge un WireBlock signé pour `wb`-comme-construit : calcule l'id canonique,
/// puis signe le message canonique. (Comme `forge_signed_wire_block_for_test`
/// mais on garde la main pour pouvoir corrompre l'id ensuite.)
fn signed_wire(
    parents: Vec<String>,
    payload: Option<PayloadEnvelope>,
    nonce: u64,
    meta: &WireMeta,
    wallet: &Wallet,
) -> WireBlock {
    let payload_json = payload.as_ref().and_then(|p| serde_json::to_string(p).ok());
    let mut wb = WireBlock {
        id: String::new(),
        parents,
        payload_json,
        nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: None,
    };
    wb.id = compute_block_id(
        &wb.parents,
        &wb.payload_json.as_ref().and_then(|s| serde_json::from_str(s).ok()),
        wb.nonce,
    );
    let msg = canonical_wireblock_message(&wb);
    wb.signature_hex = wallet.sign(&msg).expect("sign");
    wb
}

/// S4 / M-6 — un bloc dont l'`id` ≠ hash canonique du contenu est rejeté.
///
/// On corrompt l'id PUIS on re-signe le message canonique (sur l'id corrompu),
/// pour que la signature soit valide et que le rejet vienne BIEN du check d'id
/// (`persist.rs:185`), pas de la signature.
#[tokio::test]
async fn block_id_not_matching_canonical_hash_rejected() -> anyhow::Result<()> {
    let (dag, adapter, meta) = fresh("m6-id").await?;
    let wallet = Wallet::from_seed(&[30u8; 32], None).unwrap();
    let block = dag.forge_block(None, 0, compute_block_id)?;

    let mut wb = signed_wire(block.parents.clone(), block.payload.clone(), block.nonce, &meta, &wallet);
    // Corrompt l'id (syntaxiquement valide, mais ≠ contenu) puis re-signe.
    wb.id = "0".repeat(64);
    wb.signature_hex = wallet.sign(&canonical_wireblock_message(&wb)).unwrap();

    let res = adapter.persist_block(&wb).await?;
    println!("forged-id persist → {res:?}");
    match res {
        PutResult::Rejected(reason) => {
            assert!(
                reason.to_lowercase().contains("block id")
                    || reason.to_lowercase().contains("canonical"),
                "must be rejected by the id-integrity check, got: {reason}"
            );
        }
        other => panic!("a block with a forged id must be rejected, got {other:?}"),
    }
    Ok(())
}

/// S4 — idempotence : re-soumettre le MÊME bloc renvoie AlreadyExists et
/// n'applique pas l'état deux fois (la supply ne double pas).
#[tokio::test]
async fn duplicate_block_is_idempotent_no_double_apply() -> anyhow::Result<()> {
    let (dag, adapter, meta) = fresh("idem").await?;
    // Wallet minteur = "admin de test" : valide validate_mint_policy en mode dev
    // (PMS_TEST_ADMIN_PUBKEY) ; tous les tests mint de ce fichier utilisent CE
    // wallet → même valeur d'env, donc pas de race en parallèle.
    let wallet = Wallet::from_seed(&[99u8; 32], None).unwrap();
    unsafe { std::env::set_var("PMS_TEST_ADMIN_PUBKEY", wallet.encoded_public_key()) };
    let addr = wallet.get_address("8e");

    // Un Mint plain de 1000 vers `addr`. On construit le WireBlock DIRECTEMENT
    // (parent = genesis), SANS `dag.forge_block` qui pré-insère le bloc dans le
    // DAG (→ persist #1 verrait AlreadyExists). Ici persist #1 doit être Inserted.
    let genesis_id = Block::genesis(compute_block_id).id;
    let mint = PlainPayload::Mint {
        outputs: vec![TxOutput::new(addr.clone(), "1000", None)],
    };
    let wb = signed_wire(vec![genesis_id], Some(PayloadEnvelope::Plain(mint)), 5, &meta, &wallet);
    let _ = &dag; // dag gardé vivant via l'adapter

    let r1 = adapter.persist_block(&wb).await?;
    let bal1 = adapter.balance_by_address(&addr).await;
    let r2 = adapter.persist_block(&wb).await?; // MÊME bloc, re-soumis
    let bal2 = adapter.balance_by_address(&addr).await;
    println!("persist #1 → {r1:?} (bal={bal1}) | persist #2 → {r2:?} (bal={bal2})");

    assert!(matches!(r1, PutResult::Inserted | PutResult::AlreadyExists));
    assert!(
        matches!(r2, PutResult::AlreadyExists),
        "re-submitting the same block must be AlreadyExists, got {r2:?}"
    );
    assert_eq!(
        bal1, bal2,
        "re-submitting the same block must NOT double-apply the supply"
    );
    assert_eq!(bal1.to_string(), "1000", "minted balance");
    Ok(())
}

/// S4 — un bloc référençant un parent INEXISTANT est rejeté par la protection
/// `enforce_parent_existence` (réglage prod), jamais Inserted-et-appliqué.
///
/// On isole la cause du rejet (anti-faux-test rule #6) : le mint est signé par
/// le wallet "admin de test" (passe `validate_mint_policy` en dev) ET aucune
/// clé coordinateur n'est configurée en dev (passe `validate_mint_security`),
/// donc le SEUL motif de rejet possible est le parent manquant. Si la raison
/// n'est PAS "parent ... not found", le test échoue (rejet pour un mauvais
/// motif = faux test).
#[tokio::test]
async fn block_with_unknown_parent_not_silently_applied() -> anyhow::Result<()> {
    let (_dag, adapter, meta) = fresh_strict_parents("orphan").await?;
    // Même wallet admin que le test idempotence → même valeur PMS_TEST_ADMIN_PUBKEY,
    // pas de race sur l'env var en exécution parallèle.
    let wallet = Wallet::from_seed(&[99u8; 32], None).unwrap();
    unsafe { std::env::set_var("PMS_TEST_ADMIN_PUBKEY", wallet.encoded_public_key()) };
    let addr = wallet.get_address("8e");

    // Parent bidon (n'existe ni en RAM ni dans le store).
    let ghost_parent = "f".repeat(64);
    let mint = PlainPayload::Mint {
        outputs: vec![TxOutput::new(addr.clone(), "500", None)],
    };
    let payload = Some(PayloadEnvelope::Plain(mint));
    let wb = signed_wire(vec![ghost_parent], payload, 7, &meta, &wallet);

    let res = adapter.persist_block(&wb).await?;
    let bal = adapter.balance_by_address(&addr).await;
    println!("unknown-parent persist → {res:?} (bal={bal})");

    match res {
        PutResult::Rejected(reason) => {
            let r = reason.to_lowercase();
            assert!(
                r.contains("parent") && r.contains("not found"),
                "must be rejected SPECIFICALLY for the missing parent, got: {reason}"
            );
        }
        other => panic!(
            "a block with an unknown parent must be Rejected (enforce_parent_existence), got {other:?}"
        ),
    }
    assert_eq!(
        bal.to_string(),
        "0",
        "no balance must be credited from a block whose parent is missing"
    );
    Ok(())
}
