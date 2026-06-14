//! Tâche d'auto-enact de gouvernance (plan §4 / spec §6) — test direct du tick.
//!
//! `governance_enact_tick` scanne les propositions `Pending` dont le timelock est
//! écoulé (`enact_after <= now`) et les enacte. On le teste sur un `AppState`
//! testkit réel (pas de mock) :
//! - une proposition *tighten* (timelock nul, `enact_after == now`) est enactée
//!   par le tick → statut `Enacted` + `ConfigUpdate` appliqué ;
//! - une proposition *loosen* (timelock futur) n'est PAS enactée → reste `Pending`,
//!   config inchangée.
//!
//! Run : `cargo test -p pms-server --test governance_autoenact_test -- --nocapture`

use pms_config::{ConfigUpdate, GovernanceStatus, GovernanceTier};
use pms_storage::{ConfigStorage, GovernanceStorage};
use pms_testkit::make_test_state;

#[tokio::test]
async fn auto_enact_applies_elapsed_and_skips_future() -> anyhow::Result<()> {
    let (state, store, _meta) = make_test_state().await?;

    let base_maxmint = store.get_runtime_config()?.max_mint_per_block;
    let base_fee = store.get_runtime_config()?.fee_rate_bps;
    println!("baseline: max_mint_per_block={base_maxmint}, fee_rate_bps={base_fee}");
    assert_eq!(base_maxmint, 1_000_000);

    // 1) TIGHTEN (SetMaxMint=1) → timelock nul, enact_after == now (déjà éligible).
    let tighten = pms_server::api_fn::governance::do_propose(
        &state,
        ConfigUpdate::SetMaxMint { amount: 1 },
        GovernanceTier::Policy,
        "tighten".to_string(),
    )
    .await
    .expect("propose tighten");
    println!(
        "tighten proposed: id={}, instant={}, enact_after==announced={}",
        tighten.proposal_id, tighten.instant,
        tighten.enact_after_ms == tighten.announced_at_ms
    );
    assert!(tighten.instant, "tighten ⇒ timelock nul");

    // 2) LOOSEN (SetFeeRate=777) → timelock futur (7 j), PAS encore éligible.
    let loosen = pms_server::api_fn::governance::do_propose(
        &state,
        ConfigUpdate::SetFeeRate { bps: 777 },
        GovernanceTier::Operator,
        "loosen".to_string(),
    )
    .await
    .expect("propose loosen");
    println!("loosen proposed: id={}, instant={}", loosen.proposal_id, loosen.instant);
    assert!(!loosen.instant, "loosen ⇒ timelock plein (futur)");

    // Avant le tick : les deux sont Pending, rien appliqué.
    assert_eq!(
        store.get_governance_proposal(&tighten.proposal_id)?.unwrap().status,
        GovernanceStatus::Pending
    );
    assert_eq!(store.get_runtime_config()?.max_mint_per_block, 1_000_000, "rien appliqué avant le tick");

    // 3) Un passage de la tâche d'auto-enact.
    pms_server::api::governance_enact_tick(&state).await;

    // 4) Le tighten (éligible) est Enacté + appliqué ; le loosen (futur) reste Pending.
    let t_rec = store.get_governance_proposal(&tighten.proposal_id)?.unwrap();
    let l_rec = store.get_governance_proposal(&loosen.proposal_id)?.unwrap();
    let after_maxmint = store.get_runtime_config()?.max_mint_per_block;
    let after_fee = store.get_runtime_config()?.fee_rate_bps;
    println!(
        "after tick: tighten={}, loosen={}, max_mint_per_block={}, fee_rate_bps={}",
        t_rec.status.as_str(), l_rec.status.as_str(), after_maxmint, after_fee
    );

    assert_eq!(t_rec.status, GovernanceStatus::Enacted, "le tick enacte la proposition éligible (tighten)");
    assert_eq!(after_maxmint, 1, "le tick applique le ConfigUpdate du tighten (max_mint→1)");
    assert_eq!(l_rec.status, GovernanceStatus::Pending, "le tick NE touche PAS une proposition au timelock futur (loosen)");
    assert_eq!(after_fee, base_fee, "la proposition future n'est pas appliquée");

    println!("\n   PASSED: auto-enact applique les propositions éligibles, ignore les futures.");
    Ok(())
}
