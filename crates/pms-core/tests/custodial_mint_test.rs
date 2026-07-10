//! Mint custodial (protocole 2.8) — validation protocole de `PlainPayload::CustodialMint`.
//!
//! Forge directement des blocs `CustodialMint` via `persist_block` (mode dev :
//! `coordinator_public_key = None` → le gate coordinateur global du Mint n'existe
//! pas, ce qui isole EXACTEMENT ce qu'on veut tester : l'autorité déléguée par la
//! SIGNATURE du `mint_authority` embarquée dans le payload, PAS l'admin du DAG).
//!
//! Couvre : mint SFT valide, mint token valide, mauvaise autorité, anti-replay
//! (rejeu + duplicata concurrent), cap dépassé (simple ET jointement sous lock
//! per-asset), asset natif/non-enregistré rejeté, destinataire gelé rejeté,
//! `mint_authority` en bech32 (chemin custodial réel).
//!
//! Run : `PMS_CONFIG=$(pwd)/etc/config/config.dev.toml \
//!        cargo test -p pms-core --test custodial_mint_test -- --nocapture`

use std::sync::Arc;

use pms_config::load_config;
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use pms_storage::{PutResult, SftClassStorage};
use pms_testkit::forge_signed_wire_block_for_test;
use pms_types::{Block, PayloadEnvelope, PlainPayload, SftClass, TokenMetadata, TxOutput};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;
use rust_decimal::Decimal;
use std::str::FromStr;

type DagRef = Arc<ConcurrentDag>;

const HRP: &str = "8e";

async fn setup(tag: &str) -> anyhow::Result<(DagRef, Arc<dyn NetDagAdapter>, Arc<RocksStore>, WireMeta)>
{
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join(format!("rocks-cmint-{tag}"));
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
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(Block::genesis(compute_block_id)));
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone(), 0, None);
    Ok((dag, adapter, store, meta))
}

fn wallet(seed: u8) -> Wallet {
    Wallet::from_seed(&[seed; 32], None).unwrap()
}

/// Enregistre une classe SFT directement dans le store (setup — on teste le MINT,
/// pas la création). `mint_authority`/`creator` = l'adresse bech32 fournie.
fn register_sft_class(
    store: &Arc<RocksStore>,
    asset_id: &str,
    collection: &str,
    class_id: &str,
    max_supply: Option<&str>,
    authority_addr: &str,
) {
    let class = SftClass {
        asset_id: asset_id.into(),
        collection_id: collection.into(),
        class_id: class_id.into(),
        name: "Ticket".into(),
        uri: None,
        attributes: None,
        decimals: 0,
        max_supply: max_supply.map(Into::into),
        demurrage_bps_per_day: None,
        creator: authority_addr.into(),
        mint_authority: authority_addr.into(),
        royalty_bps: Some(500),
        royalty_beneficiary: Some(authority_addr.into()),
        royalty_version: 0,
    };
    store.put_sft_class(&class).expect("register sft class");
}

/// Forge un bloc `CustodialMint` signé au NIVEAU BLOC par `coord` (single-writer,
/// dev-permissif) MAIS AUTORISÉ par la signature détachée de `authority`.
#[allow(clippy::too_many_arguments)]
fn custodial_mint_wb(
    meta: &WireMeta,
    coord: &Wallet,
    wire_nonce: u64,
    parent: String,
    asset_id: &str,
    outputs: Vec<TxOutput>,
    authority: &Wallet,
    mint_nonce: &str,
) -> pms_wire::WireBlock {
    let msg = pms_types::custodial_mint_signing_message(&meta.network_id, asset_id, &outputs, mint_nonce);
    let sig = authority.sign(&msg).expect("authority sign");
    let payload = PlainPayload::CustodialMint {
        asset_id: asset_id.into(),
        outputs,
        auth_pubkey_hex: authority.public_key_hex.clone(),
        auth_signature_b64: sig,
        mint_nonce: mint_nonce.into(),
    };
    forge_signed_wire_block_for_test(
        vec![parent],
        meta,
        coord,
        wire_nonce,
        Some(PayloadEnvelope::Plain(payload)),
    )
}

/// Comme `custodial_mint_wb`, mais permet de fournir une paire (pubkey, sig)
/// EXPLICITE — utile pour tester le binding autorité (sig valide, mais d'une clé
/// qui n'est PAS le `mint_authority`).
fn custodial_mint_wb_presigned(
    meta: &WireMeta,
    coord: &Wallet,
    wire_nonce: u64,
    parent: String,
    asset_id: &str,
    outputs: Vec<TxOutput>,
    auth_pubkey_hex: String,
    auth_signature_b64: String,
    mint_nonce: &str,
) -> pms_wire::WireBlock {
    let payload = PlainPayload::CustodialMint {
        asset_id: asset_id.into(),
        outputs,
        auth_pubkey_hex,
        auth_signature_b64,
        mint_nonce: mint_nonce.into(),
    };
    forge_signed_wire_block_for_test(
        vec![parent],
        meta,
        coord,
        wire_nonce,
        Some(PayloadEnvelope::Plain(payload)),
    )
}

fn genesis() -> String {
    Block::genesis(compute_block_id).id
}

// ── 1) Mint SFT valide (mint_authority en bech32 — chemin custodial réel) ──────
#[tokio::test]
async fn custodial_mint_sft_valid_bech32_authority() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("sft-valid").await?;
    let coord = wallet(1);
    let minter = wallet(2);
    let minter_addr = minter.get_address(HRP); // bech32m — le VRAI format custodial
    println!("mint_authority (bech32) = {minter_addr}");
    register_sft_class(&store, "studio:ticket", "studio", "ticket", Some("500"), &minter_addr);

    let outputs = vec![TxOutput::new("8e1recipient", "100", Some("studio:ticket".into()))];
    let wb = custodial_mint_wb(&meta, &coord, 1, genesis(), "studio:ticket", outputs, &minter, "nonce-1");
    let r = adapter.persist_block(&wb).await?;
    println!("valid SFT custodial mint → {r:?}");
    assert!(matches!(r, PutResult::Inserted), "valid mint must be Inserted, got {r:?}");

    let bal = adapter
        .balance_by_address_and_asset("8e1recipient", Some("studio:ticket"))
        .await;
    let (supply, count) = adapter.circulating_supply_by_asset(Some("studio:ticket")).await;
    println!("recipient balance = {bal}, supply = {supply} ({count} utxo)");
    assert_eq!(bal, Decimal::from_str("100").unwrap(), "recipient must hold 100");
    assert_eq!(supply, Decimal::from_str("100").unwrap(), "supply must be 100");
    Ok(())
}

// ── 2) Mint token fongible valide (prouve le chemin unifié token + SFT) ────────
#[tokio::test]
async fn custodial_mint_token_valid() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("token-valid").await?;
    let coord = wallet(3);
    let minter = wallet(4);
    let minter_addr = minter.get_address(HRP);
    // Token fongible (asset_id SANS ':') enregistré avec mint_authority = minter.
    let tk = TokenMetadata {
        asset_id: "gold".into(),
        symbol: "GLD".into(),
        name: "Gold".into(),
        decimals: 2,
        max_supply: Some("1000".into()),
        creator: minter_addr.clone(),
        mint_authority: minter_addr.clone(),
        demurrage_bps_per_day: None,
        collateral_address: None,
        collateral_asset_id: None,
        collateral_ratio_bps: None,
        royalty_bps: None,
        royalty_beneficiary: None,
        royalty_version: 0,
    };
    store.register_token(&tk).expect("register token");

    let outputs = vec![TxOutput::new("8e1holder", "12.50", Some("gold".into()))];
    let wb = custodial_mint_wb(&meta, &coord, 1, genesis(), "gold", outputs, &minter, "n-gold");
    let r = adapter.persist_block(&wb).await?;
    println!("valid token custodial mint → {r:?}");
    assert!(matches!(r, PutResult::Inserted), "got {r:?}");

    let bal = adapter.balance_by_address_and_asset("8e1holder", Some("gold")).await;
    println!("holder gold balance = {bal}");
    assert_eq!(bal, Decimal::from_str("12.50").unwrap());
    Ok(())
}

// ── 3) Mauvaise autorité : signature valide mais PAS le mint_authority ─────────
#[tokio::test]
async fn custodial_mint_wrong_authority_rejected() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("wrong-auth").await?;
    let coord = wallet(5);
    let minter = wallet(6);
    let attacker = wallet(7); // détient une clé, mais n'est PAS le mint_authority
    register_sft_class(&store, "studio:ticket", "studio", "ticket", Some("500"), &minter.get_address(HRP));

    // L'attaquant signe correctement le message avec SA clé → signature
    // cryptographiquement valide, mais l'adresse dérivée ≠ mint_authority.
    let outputs = vec![TxOutput::new("8e1recipient", "100", Some("studio:ticket".into()))];
    let wb = custodial_mint_wb(&meta, &coord, 1, genesis(), "studio:ticket", outputs, &attacker, "nonce-x");
    let r = adapter.persist_block(&wb).await?;
    println!("wrong-authority mint → {r:?}");
    match r {
        PutResult::Rejected(reason) => assert!(
            reason.contains("mint_authority"),
            "must reject for authority mismatch, got: {reason}"
        ),
        other => panic!("expected Rejected, got {other:?}"),
    }
    let (supply, _) = adapter.circulating_supply_by_asset(Some("studio:ticket")).await;
    assert_eq!(supply, Decimal::ZERO, "nothing minted");
    Ok(())
}

// ── 3bis) Signature qui ne correspond pas à auth_pubkey_hex → rejet crypto ─────
#[tokio::test]
async fn custodial_mint_forged_signature_rejected() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("forged-sig").await?;
    let coord = wallet(8);
    let minter = wallet(9);
    register_sft_class(&store, "studio:ticket", "studio", "ticket", Some("500"), &minter.get_address(HRP));

    // auth_pubkey_hex = minter (le bon), mais signature bidon → verify échoue.
    let outputs = vec![TxOutput::new("8e1recipient", "100", Some("studio:ticket".into()))];
    let wb = custodial_mint_wb_presigned(
        &meta, &coord, 1, genesis(), "studio:ticket", outputs,
        minter.public_key_hex.clone(),
        "AAAA".into(), // signature invalide
        "nonce-y",
    );
    let r = adapter.persist_block(&wb).await?;
    println!("forged-signature mint → {r:?}");
    match r {
        PutResult::Rejected(reason) => assert!(
            reason.contains("invalid mint_authority signature"),
            "must reject invalid signature, got: {reason}"
        ),
        other => panic!("expected Rejected, got {other:?}"),
    }
    Ok(())
}

// ── 4) Anti-replay : rejouer le MÊME (asset, nonce) dans un nouveau bloc ───────
#[tokio::test]
async fn custodial_mint_replay_rejected() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("replay").await?;
    let coord = wallet(10);
    let minter = wallet(11);
    register_sft_class(&store, "studio:ticket", "studio", "ticket", Some("500"), &minter.get_address(HRP));

    let outputs = vec![TxOutput::new("8e1recipient", "50", Some("studio:ticket".into()))];
    // 1er mint (nonce "dup") → Inserted.
    let wb1 = custodial_mint_wb(&meta, &coord, 1, genesis(), "studio:ticket", outputs.clone(), &minter, "dup");
    let r1 = adapter.persist_block(&wb1).await?;
    println!("first mint (nonce=dup) → {r1:?}");
    assert!(matches!(r1, PutResult::Inserted));

    // 2e mint : MÊME nonce, bloc DIFFÉRENT (wire nonce distinct → block_id distinct
    // → pas rattrapé par l'idempotence). Doit être rejeté par l'anti-replay.
    let wb2 = custodial_mint_wb(&meta, &coord, 2, genesis(), "studio:ticket", outputs, &minter, "dup");
    assert_ne!(wb1.id, wb2.id, "les deux blocs doivent avoir des ids distincts");
    let r2 = adapter.persist_block(&wb2).await?;
    println!("replay mint (same nonce, new block) → {r2:?}");
    match r2 {
        PutResult::Rejected(reason) => assert!(
            reason.contains("consumed") || reason.contains("replay"),
            "must reject replay, got: {reason}"
        ),
        other => panic!("expected Rejected for replay, got {other:?}"),
    }

    let (supply, _) = adapter.circulating_supply_by_asset(Some("studio:ticket")).await;
    println!("supply after replay attempt = {supply}");
    assert_eq!(supply, Decimal::from_str("50").unwrap(), "replay must NOT double the supply");
    Ok(())
}

// ── 5) Duplicata CONCURRENT : deux blocs même nonce → exactement un gagne ──────
// Runtime multi-thread (parallélisme OS réel) pour exercer vraiment le claim
// atomique / le lock, pas seulement l'interleaving aux points d'await.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn custodial_mint_concurrent_same_nonce_one_winner() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("concurrent-dup").await?;
    let coord = wallet(12);
    let minter = wallet(13);
    register_sft_class(&store, "studio:ticket", "studio", "ticket", Some("500"), &minter.get_address(HRP));

    let outputs = vec![TxOutput::new("8e1recipient", "70", Some("studio:ticket".into()))];
    let wb1 = custodial_mint_wb(&meta, &coord, 1, genesis(), "studio:ticket", outputs.clone(), &minter, "race");
    let wb2 = custodial_mint_wb(&meta, &coord, 2, genesis(), "studio:ticket", outputs, &minter, "race");
    assert_ne!(wb1.id, wb2.id);

    let (r1, r2) = tokio::join!(adapter.persist_block(&wb1), adapter.persist_block(&wb2));
    let r1 = r1?;
    let r2 = r2?;
    println!("concurrent same-nonce → r1={r1:?} r2={r2:?}");
    let inserted = [&r1, &r2].iter().filter(|r| matches!(r, PutResult::Inserted)).count();
    assert_eq!(inserted, 1, "exactly one of the two duplicate mints must win");

    let (supply, _) = adapter.circulating_supply_by_asset(Some("studio:ticket")).await;
    println!("supply after concurrent dup = {supply}");
    assert_eq!(supply, Decimal::from_str("70").unwrap(), "only one 70 minted");
    Ok(())
}

// ── 6a) Cap dépassé (un seul mint > max_supply) ───────────────────────────────
#[tokio::test]
async fn custodial_mint_over_cap_rejected() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("over-cap").await?;
    let coord = wallet(14);
    let minter = wallet(15);
    register_sft_class(&store, "studio:ticket", "studio", "ticket", Some("100"), &minter.get_address(HRP));

    let outputs = vec![TxOutput::new("8e1recipient", "150", Some("studio:ticket".into()))];
    let wb = custodial_mint_wb(&meta, &coord, 1, genesis(), "studio:ticket", outputs, &minter, "big");
    let r = adapter.persist_block(&wb).await?;
    println!("over-cap mint (150 > max 100) → {r:?}");
    match r {
        PutResult::Rejected(reason) => assert!(
            reason.contains("max supply exceeded"),
            "must reject over-cap, got: {reason}"
        ),
        other => panic!("expected Rejected, got {other:?}"),
    }
    Ok(())
}

// ── 6b) Cap JOINTEMENT dépassé sous lock per-asset (anti-TOCTOU d'inflation) ───
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn custodial_mint_concurrent_joint_over_cap() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("joint-cap").await?;
    let coord = wallet(16);
    let minter = wallet(17);
    // max 100 ; deux mints de 60 (nonces DISTINCTS) : individuellement OK, mais
    // jointement 120 > 100. Le lock per-asset DOIT sérialiser lecture-cap→apply
    // pour qu'exactement un passe.
    register_sft_class(&store, "studio:ticket", "studio", "ticket", Some("100"), &minter.get_address(HRP));

    let out_a = vec![TxOutput::new("8e1a", "60", Some("studio:ticket".into()))];
    let out_b = vec![TxOutput::new("8e1b", "60", Some("studio:ticket".into()))];
    let wb_a = custodial_mint_wb(&meta, &coord, 1, genesis(), "studio:ticket", out_a, &minter, "a");
    let wb_b = custodial_mint_wb(&meta, &coord, 2, genesis(), "studio:ticket", out_b, &minter, "b");

    let (ra, rb) = tokio::join!(adapter.persist_block(&wb_a), adapter.persist_block(&wb_b));
    let ra = ra?;
    let rb = rb?;
    println!("joint-cap concurrent → ra={ra:?} rb={rb:?}");
    let inserted = [&ra, &rb].iter().filter(|r| matches!(r, PutResult::Inserted)).count();
    assert_eq!(inserted, 1, "exactly one 60-mint fits under the 100 cap");

    let (supply, _) = adapter.circulating_supply_by_asset(Some("studio:ticket")).await;
    println!("final supply = {supply} (must be ≤ 100)");
    assert_eq!(supply, Decimal::from_str("60").unwrap(), "cap must hold: only 60 minted");
    assert!(supply <= Decimal::from_str("100").unwrap());
    Ok(())
}

// ── 7) Asset natif interdit (output sans asset_id) ────────────────────────────
#[tokio::test]
async fn custodial_mint_native_output_rejected() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("native").await?;
    let coord = wallet(18);
    let minter = wallet(19);
    register_sft_class(&store, "studio:ticket", "studio", "ticket", Some("500"), &minter.get_address(HRP));

    // asset_id déclaré = classe enregistrée, mais l'output est du PMS natif (None).
    let outputs = vec![TxOutput::new("8e1recipient", "100", None)];
    let wb = custodial_mint_wb(&meta, &coord, 1, genesis(), "studio:ticket", outputs, &minter, "nat");
    let r = adapter.persist_block(&wb).await?;
    println!("native-output mint → {r:?}");
    match r {
        PutResult::Rejected(reason) => assert!(
            reason.contains("declared asset_id"),
            "must reject native/mismatched output, got: {reason}"
        ),
        other => panic!("expected Rejected, got {other:?}"),
    }
    Ok(())
}

// ── 8) Asset non enregistré (fail-closed) ─────────────────────────────────────
#[tokio::test]
async fn custodial_mint_unregistered_asset_rejected() -> anyhow::Result<()> {
    let (_dag, adapter, _store, meta) = setup("unregistered").await?;
    let coord = wallet(20);
    let minter = wallet(21);
    // Rien enregistré pour "ghost:x".
    let outputs = vec![TxOutput::new("8e1recipient", "100", Some("ghost:x".into()))];
    let wb = custodial_mint_wb(&meta, &coord, 1, genesis(), "ghost:x", outputs, &minter, "g");
    let r = adapter.persist_block(&wb).await?;
    println!("unregistered-asset mint → {r:?}");
    match r {
        PutResult::Rejected(reason) => assert!(
            reason.contains("unknown asset"),
            "must reject unregistered asset (fail-closed), got: {reason}"
        ),
        other => panic!("expected Rejected, got {other:?}"),
    }
    Ok(())
}

// ── 9) Destinataire gelé (compliance) ─────────────────────────────────────────
#[tokio::test]
async fn custodial_mint_frozen_recipient_rejected() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("frozen").await?;
    let coord = wallet(22);
    let minter = wallet(23);
    register_sft_class(&store, "studio:ticket", "studio", "ticket", Some("500"), &minter.get_address(HRP));
    // Gèle le destinataire.
    store.freeze_address("8e1frozen", "test-block", "test-freeze").expect("freeze");

    let outputs = vec![TxOutput::new("8e1frozen", "100", Some("studio:ticket".into()))];
    let wb = custodial_mint_wb(&meta, &coord, 1, genesis(), "studio:ticket", outputs, &minter, "fz");
    let r = adapter.persist_block(&wb).await?;
    println!("frozen-recipient mint → {r:?}");
    match r {
        PutResult::Rejected(reason) => assert!(
            reason.contains("frozen"),
            "must reject mint to frozen address, got: {reason}"
        ),
        other => panic!("expected Rejected, got {other:?}"),
    }
    Ok(())
}

// ── 10) CŒUR SÉCURITÉ : le coordinateur ne peut PAS altérer un output signé ────
// Le mint_authority signe des outputs précis ; le Coordinator forge le bloc. S'il
// modifie un montant/adresse/time-lock APRÈS signature, le message recalculé au
// consensus diverge → signature invalide → rejet. C'est LA garantie centrale du
// modèle custodial (le coordinateur partagé ne peut pas détourner un mint).
#[tokio::test]
async fn custodial_mint_coordinator_output_tamper_rejected() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("tamper").await?;
    let coord = wallet(24);
    let minter = wallet(25);
    register_sft_class(&store, "studio:ticket", "studio", "ticket", Some("500"), &minter.get_address(HRP));

    // Le créateur signe un mint de 100 vers "8e1legit".
    let signed_outputs = vec![TxOutput::new("8e1legit", "100", Some("studio:ticket".into()))];
    let msg = pms_types::custodial_mint_signing_message(&meta.network_id, "studio:ticket", &signed_outputs, "t1");
    let sig = minter.sign(&msg).unwrap();

    // (a) MONTANT gonflé (100 → 999) après signature.
    let tampered_amount = vec![TxOutput::new("8e1legit", "999", Some("studio:ticket".into()))];
    let wb_amt = custodial_mint_wb_presigned(
        &meta, &coord, 1, genesis(), "studio:ticket", tampered_amount,
        minter.public_key_hex.clone(), sig.clone(), "t1",
    );
    let r_amt = adapter.persist_block(&wb_amt).await?;
    println!("tampered amount (100→999) → {r_amt:?}");
    assert!(
        matches!(&r_amt, PutResult::Rejected(reason) if reason.contains("invalid mint_authority signature")),
        "amount tamper must be rejected as invalid signature, got {r_amt:?}"
    );

    // (b) DESTINATAIRE détourné vers l'adresse du coordinateur.
    let tampered_to = vec![TxOutput::new("8e1coordinator", "100", Some("studio:ticket".into()))];
    let wb_to = custodial_mint_wb_presigned(
        &meta, &coord, 2, genesis(), "studio:ticket", tampered_to,
        minter.public_key_hex.clone(), sig.clone(), "t1",
    );
    let r_to = adapter.persist_block(&wb_to).await?;
    println!("tampered recipient → {r_to:?}");
    assert!(
        matches!(&r_to, PutResult::Rejected(reason) if reason.contains("invalid mint_authority signature")),
        "recipient tamper must be rejected, got {r_to:?}"
    );

    // (c) TIME-LOCK injecté (locked_until) sur un output par ailleurs identique.
    let mut locked = TxOutput::new("8e1legit", "100", Some("studio:ticket".into()));
    locked.locked_until = Some(1893456000000);
    let wb_lock = custodial_mint_wb_presigned(
        &meta, &coord, 3, genesis(), "studio:ticket", vec![locked],
        minter.public_key_hex.clone(), sig.clone(), "t1",
    );
    let r_lock = adapter.persist_block(&wb_lock).await?;
    println!("injected time-lock → {r_lock:?}");
    assert!(
        matches!(&r_lock, PutResult::Rejected(reason) if reason.contains("invalid mint_authority signature")),
        "time-lock injection must be rejected, got {r_lock:?}"
    );

    // Rien n'a été minté par aucune tentative de tamper.
    let (supply, _) = adapter.circulating_supply_by_asset(Some("studio:ticket")).await;
    println!("supply after all tamper attempts = {supply}");
    assert_eq!(supply, Decimal::ZERO, "no tampered mint may succeed");

    // Contrôle positif : le bloc HONNÊTE (outputs == signés) passe, prouvant que
    // c'est bien le tamper — et non un rejet parasite — qui bloquait.
    let wb_ok = custodial_mint_wb_presigned(
        &meta, &coord, 4, genesis(), "studio:ticket", signed_outputs,
        minter.public_key_hex.clone(), sig, "t1",
    );
    let r_ok = adapter.persist_block(&wb_ok).await?;
    println!("honest block (untampered) → {r_ok:?}");
    assert!(matches!(r_ok, PutResult::Inserted), "honest signed mint must succeed, got {r_ok:?}");
    let bal = adapter.balance_by_address_and_asset("8e1legit", Some("studio:ticket")).await;
    assert_eq!(bal, Decimal::from_str("100").unwrap());
    Ok(())
}

// ── 11) F1 (audit) : max_supply proche de Decimal::MAX ne PANIQUE pas ──────────
// Le registry n'impose pas de plafond à `max_supply` ; un `current + mint` près de
// Decimal::MAX ferait paniquer l'addition non-checkée. Le fix `checked_add` doit
// renvoyer un rejet propre (MaxSupplyExceeded), jamais un panic du pipeline.
#[tokio::test]
async fn custodial_mint_near_decimal_max_no_panic() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("dmax").await?;
    let coord = wallet(26);
    let minter = wallet(27);
    let minter_addr = minter.get_address(HRP);
    // max_supply = Decimal::MAX. decimals=0.
    let dmax = "79228162514264337593543950335";
    register_sft_class(&store, "studio:huge", "studio", "huge", Some(dmax), &minter_addr);

    // 1er mint = Decimal::MAX → accepté (0 + MAX == MAX, pas > MAX).
    let out1 = vec![TxOutput::new("8e1r", dmax, Some("studio:huge".into()))];
    let wb1 = custodial_mint_wb(&meta, &coord, 1, genesis(), "studio:huge", out1, &minter, "m1");
    let r1 = adapter.persist_block(&wb1).await?;
    println!("mint MAX → {r1:?}");
    assert!(matches!(r1, PutResult::Inserted), "minting exactly MAX must succeed, got {r1:?}");

    // 2e mint = 1 → circulating (MAX) + 1 overflow : DOIT être un rejet propre,
    // PAS un panic. (Avant le fix : panic `attempt to add with overflow`.)
    let out2 = vec![TxOutput::new("8e1r", "1", Some("studio:huge".into()))];
    let wb2 = custodial_mint_wb(&meta, &coord, 2, genesis(), "studio:huge", out2, &minter, "m2");
    let r2 = adapter.persist_block(&wb2).await?;
    println!("mint +1 past MAX → {r2:?}");
    assert!(
        matches!(&r2, PutResult::Rejected(reason) if reason.contains("max supply exceeded")),
        "overflow must be a clean MaxSupplyExceeded reject (no panic), got {r2:?}"
    );
    Ok(())
}

// ── 12) `created_at` fourni par le client est ÉCRASÉ par le système ────────────
// Anti-antidatage : peu importe le `created_at` qu'un client met dans l'output
// (il est hors signature — cf. golden test), l'UTXO persisté porte le timestamp
// système, pas la valeur cliente.
#[tokio::test]
async fn custodial_mint_created_at_overwritten_by_system() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("created-at").await?;
    let coord = wallet(28);
    let minter = wallet(29);
    register_sft_class(&store, "studio:ticket", "studio", "ticket", Some("500"), &minter.get_address(HRP));

    // Output avec un created_at ABSURDE (année ~2035). Il ne doit PAS entrer dans
    // le message signé (normalisé à None) ni dans l'UTXO persisté.
    let mut out = TxOutput::new("8e1recipient", "100", Some("studio:ticket".into()));
    out.created_at = Some(2_100_000_000_000);
    let wb = custodial_mint_wb(&meta, &coord, 1, genesis(), "studio:ticket", vec![out], &minter, "ca");
    let r = adapter.persist_block(&wb).await?;
    println!("mint with client created_at → {r:?}");
    assert!(matches!(r, PutResult::Inserted), "got {r:?}");

    // L'UTXO persisté : created_at ≠ la valeur cliente absurde (écrasé système).
    let utxos = adapter.utxos_by_address("8e1recipient").await;
    let (_id, minted) = utxos.into_iter().find(|(_, o)| o.asset_id.as_deref() == Some("studio:ticket"))
        .expect("minted utxo present");
    println!("persisted created_at = {:?} (client posait 2_100_000_000_000)", minted.created_at);
    assert_ne!(minted.created_at, Some(2_100_000_000_000), "client created_at must be overwritten");
    assert!(minted.created_at.is_some(), "system must stamp created_at");
    Ok(())
}
