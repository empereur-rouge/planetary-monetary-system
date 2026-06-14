//! Gouvernance timelock — invariants protocole (plan §4 / `pms-spec-governance-timelock.md`).
//!
//! Ces tests forgent DIRECTEMENT des blocs `GovernanceProposal` / `GovernanceEnact`
//! pour exercer la validation `persist_block` sans passer par les endpoints :
//!
//! - **G3** : enact d'un *tighten* (timelock 0, légitimement instantané) →
//!   `apply_config_update` est exécuté, le `RuntimeConfig` reflète le changement,
//!   le statut passe `Enacted`. Prouve la moitié « apply » du timelock + l'instant
//!   du tighten (G5).
//! - **G2** : enact d'un *loosen* AVANT expiration (`now < enact_after`) →
//!   `PutResult::Rejected`, paramètre INCHANGÉ, proposition toujours `Pending`.
//! - **G4** : palier minimum imposé — proposer un paramètre Policy en tier Operator
//!   → rejeté (anti-déclassement de timelock).
//! - **DUP** : doublon de `proposal_id` → rejeté, record d'origine intact.
//!
//! Note : pas de test « anti-backdating » — il n'y a volontairement PAS de check
//! `announced_at ≈ horloge` (il casserait le sync P2P / replay, non-déterministe).
//! La validation `enact_after == announced_at + durée` reste, elle, déterministe.
//!
//! En mode dev (`coordinator_public_key = None`) la vérification d'autorité
//! coordinator est court-circuitée (cf. `authority.rs`), on isole donc la logique
//! de gouvernance, pas l'auth.
//!
//! Run : `cargo test -p pms-core --test governance_timelock_test -- --nocapture`

use std::sync::Arc;

use pms_config::{ConfigUpdate, GovernanceStatus, GovernanceTier, load_config};
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use pms_storage::{ConfigStorage, GovernanceStorage, PutResult};
use pms_testkit::forge_signed_wire_block_for_test;
use pms_types::{Block, PayloadEnvelope, PlainPayload};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;

type DagRef = Arc<ConcurrentDag>;

const DAY_MS: u64 = 86_400_000;

/// Store Rocks éphémère + DAG genesis + adapter réel (policy dev : pas de
/// coordinateur configuré → autorité skip ; `enforce_parent_existence = false`).
/// Renvoie aussi le `store` (Arc partagé avec l'adapter) pour inspecter l'état
/// APRÈS persist (runtime config + statut de proposition).
async fn setup(tag: &str) -> anyhow::Result<(DagRef, Arc<dyn NetDagAdapter>, Arc<RocksStore>, WireMeta)>
{
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join(format!("rocks-gov-{tag}"));
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
    std::mem::forget(dir); // garde le tempdir vivant le temps du test
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(Block::genesis(compute_block_id)));
    let adapter: Arc<dyn NetDagAdapter> =
        CoreAdapter::new(dag.clone(), store.clone(), 0, None);
    Ok((dag, adapter, store, meta))
}

/// G3 + G5 — enact d'un *tighten* (timelock instantané) applique réellement le
/// `ConfigUpdate`.
///
/// `SetMaxMint { amount: 1 }` avec la config par défaut (`max_mint_per_block =
/// 1_000_000`) est un **tighten** (on baisse le plafond) → timelock 0 →
/// `enact_after == announced_at`. L'enact immédiat est donc LÉGITIME (pas de
/// back-dating) et applique : `max_mint_per_block` passe à 1, statut `Enacted`.
#[tokio::test]
async fn governance_tighten_enacts_instantly_and_applies() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("g3").await?;
    let wallet = Wallet::from_seed(&[42u8; 32], None).unwrap();
    let genesis_id = Block::genesis(compute_block_id).id;

    let before = store.get_runtime_config()?.max_mint_per_block;
    assert_eq!(before, 1_000_000, "baseline max_mint_per_block (dev default)");
    println!("   baseline max_mint_per_block = {before}");

    // Tighten : enact_after = announced_at (timelock 0). announced_at = now (ancré).
    let now = pms_utils::ts_ms();
    let proposal_id = "g3-tighten-maxmint".to_string();
    let proposal = PlainPayload::GovernanceProposal {
        proposal_id: proposal_id.clone(),
        update: ConfigUpdate::SetMaxMint { amount: 1 },
        tier: GovernanceTier::Policy, // SetMaxMint exige au moins Policy
        reason: "tighten per-block mint cap (emergency)".to_string(),
        announced_at_ms: now,
        enact_after_ms: now, // tighten ⇒ instantané
    };
    let wb_prop =
        forge_signed_wire_block_for_test(vec![genesis_id], &meta, &wallet, 1, Some(PayloadEnvelope::Plain(proposal)));
    let r_prop = adapter.persist_block(&wb_prop).await?;
    println!("   proposal persist → {r_prop:?}");
    assert!(matches!(r_prop, PutResult::Inserted), "proposal must be inserted, got {r_prop:?}");
    let rec = store.get_governance_proposal(&proposal_id)?.expect("stored");
    assert_eq!(rec.enact_after_ms, rec.announced_at_ms, "tighten ⇒ enact_after == announced_at (instant)");

    // Enact immédiat → applique (now ≥ enact_after, légitime).
    let enact = PlainPayload::GovernanceEnact {
        proposal_id: proposal_id.clone(),
        reason: "enact tighten".to_string(),
    };
    let wb_enact =
        forge_signed_wire_block_for_test(vec![wb_prop.id.clone()], &meta, &wallet, 2, Some(PayloadEnvelope::Plain(enact)));
    let r_enact = adapter.persist_block(&wb_enact).await?;
    println!("   enact persist → {r_enact:?}");
    assert!(matches!(r_enact, PutResult::Inserted), "instant tighten enact must be inserted, got {r_enact:?}");

    let after = store.get_runtime_config()?.max_mint_per_block;
    let status = store.get_governance_proposal(&proposal_id)?.expect("proposal").status;
    println!("   after enact: max_mint_per_block = {after}, status = {}", status.as_str());
    assert_eq!(after, 1, "G3/G5: tighten enact MUST apply the ConfigUpdate (max_mint_per_block)");
    assert_eq!(status, GovernanceStatus::Enacted, "G3: proposal status MUST become Enacted");

    println!("\n   G3/G5 PASSED: tighten enacté instantanément applique le ConfigUpdate + Enacted.");
    Ok(())
}

/// G2 — enact d'un *loosen* AVANT expiration est REJETÉ, sans application.
///
/// `SetFeeRate` (loosen, palier Operator → 7 j) : `enact_after = announced_at +
/// 7 j`. L'enact immédiat (`now < enact_after`) → `Rejected("timelock")`.
#[tokio::test]
async fn governance_loosen_enact_before_timelock_rejected() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("g2").await?;
    let wallet = Wallet::from_seed(&[43u8; 32], None).unwrap();
    let genesis_id = Block::genesis(compute_block_id).id;

    let before = store.get_runtime_config()?.fee_rate_bps;
    const NEW_BPS: u32 = 9191;
    assert_ne!(before, NEW_BPS);
    println!("   baseline fee_rate_bps = {before}");

    let now = pms_utils::ts_ms();
    let proposal_id = "g2-loosen-feerate".to_string();
    let proposal = PlainPayload::GovernanceProposal {
        proposal_id: proposal_id.clone(),
        update: ConfigUpdate::SetFeeRate { bps: NEW_BPS },
        tier: GovernanceTier::Operator,
        reason: "raise fee (loosen, 7d timelock)".to_string(),
        announced_at_ms: now,
        enact_after_ms: now + 7 * DAY_MS, // loosen Operator ⇒ 7 j
    };
    let wb_prop =
        forge_signed_wire_block_for_test(vec![genesis_id], &meta, &wallet, 1, Some(PayloadEnvelope::Plain(proposal)));
    assert!(matches!(adapter.persist_block(&wb_prop).await?, PutResult::Inserted));

    let enact = PlainPayload::GovernanceEnact {
        proposal_id: proposal_id.clone(),
        reason: "enact too early".to_string(),
    };
    let wb_enact =
        forge_signed_wire_block_for_test(vec![wb_prop.id.clone()], &meta, &wallet, 2, Some(PayloadEnvelope::Plain(enact)));
    let r_enact = adapter.persist_block(&wb_enact).await?;
    println!("   early enact persist → {r_enact:?}");
    match r_enact {
        PutResult::Rejected(reason) => assert!(
            reason.to_lowercase().contains("timelock"),
            "G2: enact must be rejected SPECIFICALLY for the timelock, got: {reason}"
        ),
        other => panic!("G2: enact before timelock MUST be Rejected, got {other:?}"),
    }

    let after = store.get_runtime_config()?.fee_rate_bps;
    let status = store.get_governance_proposal(&proposal_id)?.expect("proposal").status;
    println!("   after rejected enact: fee_rate_bps = {after}, status = {}", status.as_str());
    assert_eq!(after, before, "G2: a rejected enact MUST NOT change the parameter");
    assert_eq!(status, GovernanceStatus::Pending, "G2: a rejected enact leaves the proposal Pending");

    println!("\n   G2 PASSED: loosen enact avant expiration REJETÉ (timelock), config inchangée.");
    Ok(())
}

/// G4 — palier minimum imposé : un paramètre Policy proposé en tier Operator est
/// rejeté (on ne peut pas déclasser le timelock).
#[tokio::test]
async fn governance_tier_below_minimum_rejected() -> anyhow::Result<()> {
    let (_dag, adapter, _store, meta) = setup("g4").await?;
    let wallet = Wallet::from_seed(&[45u8; 32], None).unwrap();
    let genesis_id = Block::genesis(compute_block_id).id;

    // SetBurnRate exige au moins Policy ; on le propose en Operator → rejet.
    let now = pms_utils::ts_ms();
    let proposal = PlainPayload::GovernanceProposal {
        proposal_id: "g4-burn-as-operator".to_string(),
        update: ConfigUpdate::SetBurnRate { bps: 1234 },
        tier: GovernanceTier::Operator, // trop bas pour SetBurnRate (min Policy)
        reason: "sneak a Policy change through Operator".to_string(),
        announced_at_ms: now,
        enact_after_ms: now + 7 * DAY_MS,
    };
    let wb =
        forge_signed_wire_block_for_test(vec![genesis_id], &meta, &wallet, 1, Some(PayloadEnvelope::Plain(proposal)));
    let r = adapter.persist_block(&wb).await?;
    println!("   under-tier proposal persist → {r:?}");
    match r {
        PutResult::Rejected(reason) => assert!(
            reason.to_lowercase().contains("requires at least tier policy"),
            "G4: must be rejected for the min-tier, naming the required tier, got: {reason}"
        ),
        other => panic!("G4: a Policy param proposed as Operator MUST be Rejected, got {other:?}"),
    }

    println!("\n   G4 PASSED: palier minimum imposé (SetBurnRate en Operator rejeté).");
    Ok(())
}

/// DUP — un second `GovernanceProposal` réutilisant un `proposal_id` connu est
/// rejeté au niveau DAG (pas d'overwrite aveugle), record d'origine intact.
#[tokio::test]
async fn governance_duplicate_proposal_id_rejected_no_overwrite() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("dup").await?;
    let wallet = Wallet::from_seed(&[44u8; 32], None).unwrap();
    let genesis_id = Block::genesis(compute_block_id).id;
    let proposal_id = "dup-proposal-feerate".to_string();
    let now = pms_utils::ts_ms();

    let p1 = PlainPayload::GovernanceProposal {
        proposal_id: proposal_id.clone(),
        update: ConfigUpdate::SetFeeRate { bps: 1111 },
        tier: GovernanceTier::Operator,
        reason: "original".to_string(),
        announced_at_ms: now,
        enact_after_ms: now + 7 * DAY_MS,
    };
    let wb1 =
        forge_signed_wire_block_for_test(vec![genesis_id.clone()], &meta, &wallet, 1, Some(PayloadEnvelope::Plain(p1)));
    assert!(matches!(adapter.persist_block(&wb1).await?, PutResult::Inserted));

    // Doublon : MÊME proposal_id, contenu différent → REJETÉ.
    let now2 = pms_utils::ts_ms();
    let p2 = PlainPayload::GovernanceProposal {
        proposal_id: proposal_id.clone(),
        update: ConfigUpdate::SetFeeRate { bps: 2222 },
        tier: GovernanceTier::Operator,
        reason: "attempted overwrite".to_string(),
        announced_at_ms: now2,
        enact_after_ms: now2 + 7 * DAY_MS,
    };
    let wb2 =
        forge_signed_wire_block_for_test(vec![wb1.id.clone()], &meta, &wallet, 2, Some(PayloadEnvelope::Plain(p2)));
    let r2 = adapter.persist_block(&wb2).await?;
    println!("   duplicate proposal persist → {r2:?}");
    match r2 {
        PutResult::Rejected(reason) => assert!(
            reason.to_lowercase().contains("already exists"),
            "must be rejected SPECIFICALLY for the duplicate id, got: {reason}"
        ),
        other => panic!("duplicate proposal_id MUST be Rejected, got {other:?}"),
    }

    let rec = store.get_governance_proposal(&proposal_id)?.expect("original record");
    println!("   original record after dup: reason={:?}", rec.reason);
    assert_eq!(rec.reason, "original", "the original proposal must NOT be overwritten");
    assert_eq!(rec.status, GovernanceStatus::Pending);

    println!("\n   DUP PASSED: doublon de proposal_id rejeté, record d'origine intact.");
    Ok(())
}
