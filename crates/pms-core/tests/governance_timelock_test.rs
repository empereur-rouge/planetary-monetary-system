//! Gouvernance timelock — invariant protocole (plan §4 / `pms-spec-governance-timelock.md`).
//!
//! Ces tests forgent DIRECTEMENT des blocs `GovernanceProposal` / `GovernanceEnact`
//! avec un `enact_after_ms` CONTRÔLÉ (passé ou futur), ce que les endpoints HTTP
//! ne permettent pas (les durées y sont hardcodées 7/15/45 j). Ils prouvent les
//! deux moitiés de l'invariant timelock au niveau `persist_block` :
//!
//! - **G3** : enact APRÈS expiration (`now ≥ enact_after`) → `apply_config_update`
//!   est exécuté, le `RuntimeConfig` reflète le changement, le statut passe `Enacted`.
//! - **G2** : enact AVANT expiration (`now < enact_after`) → `PutResult::Rejected`,
//!   le paramètre est INCHANGÉ, la proposition reste `Pending` (aucune application).
//!
//! En mode dev (`coordinator_public_key = None`) la vérification d'autorité
//! coordinator est court-circuitée (cf. `authority.rs`), donc n'importe quel wallet
//! peut forger le bloc — on isole ainsi la logique de timelock, pas l'auth.
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

const FAR_FUTURE_MS: u64 = 32_503_680_000_000; // ~ an 3000 — jamais écoulé en test.

/// G3 — enact APRÈS expiration applique réellement le `ConfigUpdate`.
///
/// Scénario : on annonce une proposition dont le timelock est DÉJÀ écoulé
/// (`enact_after_ms = 1`, soit 1970), puis on l'enacte. Le `fee_rate_bps` du
/// `RuntimeConfig` doit passer à la valeur proposée (golden 4242) et le statut
/// de la proposition doit devenir `Enacted`.
#[tokio::test]
async fn governance_enact_after_timelock_applies_config() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("g3").await?;
    let wallet = Wallet::from_seed(&[42u8; 32], None).unwrap();
    let genesis_id = Block::genesis(compute_block_id).id;

    // Baseline : la valeur AVANT enact ≠ la valeur proposée (sinon le test serait
    // une tautologie — il passerait même sans application).
    let before = store.get_runtime_config()?.fee_rate_bps;
    const NEW_BPS: u32 = 4242;
    assert_ne!(before, NEW_BPS, "baseline fee_rate_bps must differ from the proposed value");
    println!("   baseline fee_rate_bps = {before}");

    // 1) Proposal avec timelock DÉJÀ écoulé (enact_after = 1 ms après epoch).
    let proposal_id = "g3-proposal-feerate".to_string();
    let proposal = PlainPayload::GovernanceProposal {
        proposal_id: proposal_id.clone(),
        update: ConfigUpdate::SetFeeRate { bps: NEW_BPS },
        tier: GovernanceTier::Operator,
        reason: "raise fee (timelock already elapsed)".to_string(),
        announced_at_ms: 1,
        enact_after_ms: 1,
    };
    let wb_prop =
        forge_signed_wire_block_for_test(vec![genesis_id], &meta, &wallet, 1, Some(PayloadEnvelope::Plain(proposal)));
    let r_prop = adapter.persist_block(&wb_prop).await?;
    println!("   proposal persist → {r_prop:?}");
    assert!(matches!(r_prop, PutResult::Inserted), "proposal must be inserted, got {r_prop:?}");
    let rec = store.get_governance_proposal(&proposal_id)?.expect("proposal stored");
    assert_eq!(rec.status, GovernanceStatus::Pending, "freshly announced → Pending");

    // 2) Enact : now (2026) ≥ enact_after (1) → applique.
    let enact = PlainPayload::GovernanceEnact {
        proposal_id: proposal_id.clone(),
        reason: "enact now".to_string(),
    };
    let wb_enact =
        forge_signed_wire_block_for_test(vec![wb_prop.id.clone()], &meta, &wallet, 2, Some(PayloadEnvelope::Plain(enact)));
    let r_enact = adapter.persist_block(&wb_enact).await?;
    println!("   enact persist → {r_enact:?}");
    assert!(matches!(r_enact, PutResult::Inserted), "enact after timelock must be inserted, got {r_enact:?}");

    // 3) Vérification de l'APPLICATION réelle (G3) — valeurs golden, pas re-dérivées.
    let after = store.get_runtime_config()?.fee_rate_bps;
    let status = store.get_governance_proposal(&proposal_id)?.expect("proposal").status;
    println!("   after enact: fee_rate_bps = {after}, status = {}", status.as_str());
    assert_eq!(after, NEW_BPS, "G3: enact after timelock MUST apply the ConfigUpdate (fee_rate_bps)");
    assert_eq!(status, GovernanceStatus::Enacted, "G3: proposal status MUST become Enacted");

    println!("\n   G3 PASSED: enact après expiration applique le ConfigUpdate + marque Enacted.");
    Ok(())
}

/// G2 — enact AVANT expiration est REJETÉ, sans aucune application (timelock
/// inviolable au niveau protocole).
///
/// Scénario : proposition avec `enact_after_ms` très loin dans le futur, enact
/// immédiat → `PutResult::Rejected("timelock not elapsed")`. Le `fee_rate_bps`
/// est INCHANGÉ et la proposition reste `Pending`.
#[tokio::test]
async fn governance_enact_before_timelock_rejected_no_apply() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("g2").await?;
    let wallet = Wallet::from_seed(&[43u8; 32], None).unwrap();
    let genesis_id = Block::genesis(compute_block_id).id;

    let before = store.get_runtime_config()?.fee_rate_bps;
    const NEW_BPS: u32 = 9191;
    assert_ne!(before, NEW_BPS);
    println!("   baseline fee_rate_bps = {before}");

    // 1) Proposal avec timelock LOIN dans le futur.
    let proposal_id = "g2-proposal-feerate".to_string();
    let proposal = PlainPayload::GovernanceProposal {
        proposal_id: proposal_id.clone(),
        update: ConfigUpdate::SetFeeRate { bps: NEW_BPS },
        tier: GovernanceTier::Constitution,
        reason: "raise fee (timelock NOT elapsed)".to_string(),
        announced_at_ms: 1,
        enact_after_ms: FAR_FUTURE_MS,
    };
    let wb_prop =
        forge_signed_wire_block_for_test(vec![genesis_id], &meta, &wallet, 1, Some(PayloadEnvelope::Plain(proposal)));
    let r_prop = adapter.persist_block(&wb_prop).await?;
    println!("   proposal persist → {r_prop:?}");
    assert!(matches!(r_prop, PutResult::Inserted));

    // 2) Enact immédiat → REJETÉ (timelock non écoulé). On isole la raison du rejet
    //    (anti-faux-test rule #6) : ce doit être le timelock, pas un statut/lookup.
    let enact = PlainPayload::GovernanceEnact {
        proposal_id: proposal_id.clone(),
        reason: "enact too early".to_string(),
    };
    let wb_enact =
        forge_signed_wire_block_for_test(vec![wb_prop.id.clone()], &meta, &wallet, 2, Some(PayloadEnvelope::Plain(enact)));
    let r_enact = adapter.persist_block(&wb_enact).await?;
    println!("   early enact persist → {r_enact:?}");
    match r_enact {
        PutResult::Rejected(reason) => {
            assert!(
                reason.to_lowercase().contains("timelock"),
                "G2: enact must be rejected SPECIFICALLY for the timelock, got: {reason}"
            );
        }
        other => panic!("G2: enact before timelock MUST be Rejected, got {other:?}"),
    }

    // 3) Aucune application : config inchangée + proposition toujours Pending.
    let after = store.get_runtime_config()?.fee_rate_bps;
    let status = store.get_governance_proposal(&proposal_id)?.expect("proposal").status;
    println!("   after rejected enact: fee_rate_bps = {after}, status = {}", status.as_str());
    assert_eq!(after, before, "G2: a rejected enact MUST NOT change the parameter");
    assert_eq!(status, GovernanceStatus::Pending, "G2: a rejected enact leaves the proposal Pending");

    println!("\n   G2 PASSED: enact avant expiration REJETÉ (timelock), aucun changement de config.");
    Ok(())
}

/// Unicité du `proposal_id` — un second `GovernanceProposal` réutilisant un id
/// déjà connu est REJETÉ au niveau DAG (pas d'overwrite aveugle du record).
///
/// `put_governance_proposal` est un write aveugle ; la garde d'unicité vit dans
/// `persist_block` (source de vérité). On vérifie que le record d'origine
/// (statut/raison) est INTACT après la tentative de doublon.
#[tokio::test]
async fn governance_duplicate_proposal_id_rejected_no_overwrite() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("dup").await?;
    let wallet = Wallet::from_seed(&[44u8; 32], None).unwrap();
    let genesis_id = Block::genesis(compute_block_id).id;
    let proposal_id = "dup-proposal-feerate".to_string();

    // 1) Première proposition → stockée Pending.
    let p1 = PlainPayload::GovernanceProposal {
        proposal_id: proposal_id.clone(),
        update: ConfigUpdate::SetFeeRate { bps: 1111 },
        tier: GovernanceTier::Operator,
        reason: "original".to_string(),
        announced_at_ms: 1,
        enact_after_ms: FAR_FUTURE_MS,
    };
    let wb1 =
        forge_signed_wire_block_for_test(vec![genesis_id.clone()], &meta, &wallet, 1, Some(PayloadEnvelope::Plain(p1)));
    assert!(matches!(adapter.persist_block(&wb1).await?, PutResult::Inserted));

    // 2) Doublon : MÊME proposal_id, contenu différent → REJETÉ.
    let p2 = PlainPayload::GovernanceProposal {
        proposal_id: proposal_id.clone(),
        update: ConfigUpdate::SetFeeRate { bps: 2222 },
        tier: GovernanceTier::Constitution,
        reason: "attempted overwrite".to_string(),
        announced_at_ms: 2,
        enact_after_ms: 2,
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

    // 3) Le record d'origine est INTACT (pas d'overwrite par le doublon).
    let rec = store.get_governance_proposal(&proposal_id)?.expect("original record");
    println!("   original record after dup: reason={:?}, tier={}", rec.reason, rec.tier.as_str());
    assert_eq!(rec.reason, "original", "the original proposal must NOT be overwritten");
    assert_eq!(rec.tier, GovernanceTier::Operator, "tier must remain the original");
    assert_eq!(rec.status, GovernanceStatus::Pending);

    println!("\n   DUP PASSED: doublon de proposal_id rejeté, record d'origine intact.");
    Ok(())
}
