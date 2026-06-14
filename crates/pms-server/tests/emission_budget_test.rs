//! Tests du gate d'émission (plan §3.1) — propriétés de sécurité P1 (plafond)
//! et P2 (exactement-une-fois / crash-consistency).
//!
//! Ces tests appellent le VRAI `EmissionGate` contre un VRAI `RocksStore`
//! (tempdir) — pas de mock, pas de copie de la logique de prod. Chaque test
//! affiche ses valeurs clés (`--nocapture`) et asserte des goldens hardcodés.
//!
//! Lancer : `cargo test -p pms-server --test emission_budget_test -- --nocapture`

use pms_config::ConfigUpdate;
use pms_server::emission::{EmissionGate, EmissionParams, EmissionError, Voie};
use pms_storage::ConfigStorage;
use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use rust_decimal::Decimal;
use std::sync::Arc;

/// Période de référence : epoch #100 d'un découpage journalier.
const EPOCH_DUR_SEC: u64 = 86_400;
const NOW_EPOCH_100: u64 = 100 * 86_400_000; // ms → epoch_id = 100
const NOW_EPOCH_101: u64 = 101 * 86_400_000; // epoch suivant

fn dec(s: &str) -> Decimal {
    Decimal::from_str_exact(s).unwrap()
}

/// Paramètres standard : cible 2 %/an, plafond 10 %, plancher 0, epoch 1 jour.
fn std_params() -> EmissionParams {
    EmissionParams {
        target_pct: 2.0,
        ceiling_pct: 10.0,
        floor_pct: 0.0,
        epoch_duration_sec: EPOCH_DUR_SEC,
    }
}

/// supply 1000 × 2 % / 365 = 0.05479452 (le budget journalier de référence).
fn ref_budget() -> Decimal {
    dec("0.05479452")
}

async fn temp_store() -> (Arc<RocksStore>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("emission-rocks");
    let store = RocksStore::new(
        path.to_string_lossy().as_ref(),
        1024,
        "pms:test",
        None,
        &RocksMemoryConfig::default(),
    )
    .await
    .unwrap();
    store.ensure_schema().await.unwrap();
    (Arc::new(store), tmp)
}

/// T4 — Épuisement → rejet (P1). Un mint qui dépasse le budget est REFUSÉ
/// (pas clampé en silence), et rien n'est réservé. La raison du rejet est
/// assertée (variant `BudgetExhausted`), pas juste « une erreur a eu lieu ».
#[tokio::test]
async fn t4_exhaustion_rejects_and_reserves_nothing() {
    let (store, _tmp) = temp_store().await;
    let gate = EmissionGate::new(Default::default());

    // Demande 0.1 alors que le budget de la période n'est que 0.05479452.
    let res = gate
        .reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), Voie::OnRamp, Some(dec("0.1")))
        .await;
    println!("T4 reserve(0.1) over budget {} → {:?}", ref_budget(), res);
    assert!(
        matches!(res, Err(EmissionError::BudgetExhausted { .. })),
        "un mint au-delà du budget DOIT être rejeté (BudgetExhausted)"
    );

    // Rien n'a été réservé : le compteur est resté à 0.
    let snap = gate.snapshot().await;
    println!("T4 after reject: emitted={}, budget={}", snap.emitted, snap.budget);
    assert_eq!(snap.emitted, Decimal::ZERO, "un rejet ne consomme aucun budget");
    assert_eq!(snap.budget, ref_budget(), "budget de période = couloir × supply × frac");

    // Une demande dans le budget passe ensuite.
    let ok = gate
        .reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), Voie::OnRamp, Some(dec("0.01")))
        .await
        .expect("0.01 < budget doit passer");
    println!("T4 reserve(0.01) within budget → amount={}", ok.amount);
    assert_eq!(ok.amount, dec("0.01"));
    assert_eq!(gate.snapshot().await.emitted, dec("0.01"));
}

/// T5 — TOCTOU fermé (P1). Deux réservations concurrentes dont la somme dépasse
/// le budget : exactement UNE réussit, l'autre est rejetée, et la supply finale
/// réservée ne dépasse jamais le budget. C'est l'invariant que le chemin de
/// forge concurrent (sans lock global) ne pouvait PAS garantir avant le gate.
#[tokio::test]
async fn t5_concurrent_reserves_close_toctou() {
    let (store, _tmp) = temp_store().await;
    let gate = Arc::new(EmissionGate::new(Default::default()));

    // budget = 0.05479452 ; deux demandes de 0.04 (somme 0.08 > budget).
    let (g1, s1) = (gate.clone(), store.clone());
    let (g2, s2) = (gate.clone(), store.clone());
    let h1 = tokio::spawn(async move {
        g1.reserve(&s1, NOW_EPOCH_100, dec("1000"), std_params(), Voie::OnRamp, Some(dec("0.04"))).await
    });
    let h2 = tokio::spawn(async move {
        g2.reserve(&s2, NOW_EPOCH_100, dec("1000"), std_params(), Voie::OnRamp, Some(dec("0.04"))).await
    });
    let (r1, r2) = (h1.await.unwrap(), h2.await.unwrap());

    let oks = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
    let errs = [&r1, &r2].iter().filter(|r| r.is_err()).count();
    let snap = gate.snapshot().await;
    println!(
        "T5 concurrent 2×0.04 on budget {}: oks={}, errs={}, final emitted={}",
        snap.budget, oks, errs, snap.emitted
    );
    assert_eq!(oks, 1, "exactement une réservation concurrente doit réussir");
    assert_eq!(errs, 1, "l'autre doit être rejetée (budget insuffisant)");
    assert!(snap.emitted <= snap.budget, "P1 : émis ≤ budget, jamais de dépassement");
    assert_eq!(snap.emitted, dec("0.04"), "exactement un mint de 0.04 réservé");
}

/// T7 — Crash-consistency (P2). L'état est rechargé à l'identique depuis le
/// store (recovery au boot) — on ne re-somme jamais les blocs. Après reload, le
/// compteur de la même période n'est PAS réinitialisé.
#[tokio::test]
async fn t7_reload_from_store_reconstructs_state() {
    let (store, _tmp) = temp_store().await;

    // Première vie : réserve 0.03.
    let gate1 = EmissionGate::new(Default::default());
    gate1
        .reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), Voie::OnRamp, Some(dec("0.03")))
        .await
        .unwrap();
    let before = gate1.snapshot().await;
    println!("T7 before crash: epoch={}, budget={}, emitted={}", before.epoch_id, before.budget, before.emitted);
    drop(gate1);

    // Deuxième vie : recharge depuis le store.
    let gate2 = EmissionGate::load(&store);
    let after = gate2.snapshot().await;
    println!("T7 after reload: epoch={}, budget={}, emitted={}", after.epoch_id, after.budget, after.emitted);
    assert_eq!(after, before, "l'état rechargé doit être identique (P2)");
    assert_eq!(after.emitted, dec("0.03"));

    // Même période après reload → pas de reset, le restant tient compte du déjà-émis.
    let r = gate2
        .reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), Voie::OnRamp, Some(dec("0.01")))
        .await
        .unwrap();
    println!("T7 post-reload reserve(0.01) → amount={}, emitted={}", r.amount, gate2.snapshot().await.emitted);
    assert_eq!(gate2.snapshot().await.emitted, dec("0.04"), "0.03 (rechargé) + 0.01");
}

/// T8 — Rollover d'epoch. Le budget se réinitialise au changement de période
/// (forward), mais une horloge qui RECULE ne réinitialise jamais le compteur
/// (sinon ré-émission = dépassement).
#[tokio::test]
async fn t8_epoch_rollover_forward_only() {
    let (store, _tmp) = temp_store().await;
    let gate = EmissionGate::new(Default::default());

    // Epoch 100 : la baseline minte tout le budget (résidu = budget).
    let r100 = gate
        .reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), Voie::Baseline, None)
        .await
        .unwrap();
    println!("T8 epoch100 baseline amount={}, emitted={}", r100.amount, gate.snapshot().await.emitted);
    assert_eq!(r100.amount, ref_budget());
    assert_eq!(gate.snapshot().await.emitted, ref_budget());

    // Epoch 101 : rollover → compteur réinitialisé, nouveau budget.
    let r101 = gate
        .reserve(&store, NOW_EPOCH_101, dec("1000"), std_params(), Voie::Baseline, None)
        .await
        .unwrap();
    let snap101 = gate.snapshot().await;
    println!("T8 epoch101 baseline amount={}, epoch={}, emitted={}", r101.amount, snap101.epoch_id, snap101.emitted);
    assert_eq!(snap101.epoch_id, 101, "rollover vers l'epoch 101");
    assert_eq!(r101.amount, ref_budget(), "budget frais à la nouvelle période");

    // Horloge qui recule (epoch 100) → PAS de reset : le budget 101 est épuisé.
    let back = gate
        .reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), Voie::OnRamp, Some(dec("0.001")))
        .await;
    println!("T8 backward clock reserve → {:?} (must be rejected, no reset)", back);
    assert!(
        matches!(back, Err(EmissionError::BudgetExhausted { .. })),
        "une horloge qui recule ne DOIT pas réinitialiser le budget"
    );
    assert_eq!(gate.snapshot().await.epoch_id, 101, "epoch_id ne recule jamais");
}

/// T3 — La baseline minte le RÉSIDU. Une voie consomme une partie du budget,
/// puis la baseline complète exactement jusqu'à la cible (`budget − émis`).
#[tokio::test]
async fn t3_baseline_mints_residual() {
    let (store, _tmp) = temp_store().await;
    let gate = EmissionGate::new(Default::default());

    // Voie A émet 0.02 dans la période.
    gate.reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), Voie::OnRamp, Some(dec("0.02")))
        .await
        .unwrap();

    // La baseline complète : résidu = budget − 0.02 = 0.03479452.
    let base = gate
        .reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), Voie::Baseline, None)
        .await
        .unwrap();
    let snap = gate.snapshot().await;
    println!(
        "T3 onramp 0.02 + baseline residual {} → total emitted={} (budget={})",
        base.amount, snap.emitted, snap.budget
    );
    assert_eq!(base.amount, dec("0.03479452"), "résidu = budget − déjà-émis");
    assert_eq!(snap.emitted, ref_budget(), "total période = budget cible exact");

    // Une 2e baseline dans la même période ne minte plus rien (budget consommé).
    let base2 = gate
        .reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), Voie::Baseline, None)
        .await
        .unwrap();
    println!("T3 second baseline same epoch → amount={}", base2.amount);
    assert_eq!(base2.amount, Decimal::ZERO, "budget déjà consommé → 0, pas de bloc vide");
}

/// T10 — Rollback sur échec de persistance du bloc. `release` annule la
/// réservation (le budget consommé revient), de façon durable.
#[tokio::test]
async fn t10_release_rolls_back_reservation() {
    let (store, _tmp) = temp_store().await;
    let gate = EmissionGate::new(Default::default());

    gate.reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), Voie::OnRamp, Some(dec("0.03")))
        .await
        .unwrap();
    assert_eq!(gate.snapshot().await.emitted, dec("0.03"));

    // Simule un échec de persist_block après réservation : on relâche.
    gate.release(&store, dec("0.03")).await;
    let snap = gate.snapshot().await;
    println!("T10 after release: emitted={}", snap.emitted);
    assert_eq!(snap.emitted, Decimal::ZERO, "release rend le budget réservé");

    // Le rollback est durable : un reload depuis le store voit emitted=0.
    let reloaded = EmissionGate::load(&store).snapshot().await;
    println!("T10 reloaded emitted={}", reloaded.emitted);
    assert_eq!(reloaded.emitted, Decimal::ZERO, "rollback persisté (P2)");
}

/// T11 / G6 — kill-switch `mint_enabled = false` (gouvernance) refuse TOUTE voie
/// d'émission de PMS natif, AVANT toute réservation (rien n'est consommé).
///
/// Câble enfin le champ `mint_enabled` (dormant jusqu'à v0.16.0 P2b) : armé via
/// un `ConfigUpdate::SetMintEnabled{false}` (le chemin réel, gouverné), il fait
/// échouer `EmissionGate::reserve` avec `MintDisabled` pour baseline / on-ramp /
/// conversion / faucet. La réactivation (gouvernance) rouvre le mint.
#[tokio::test]
async fn t11_mint_disabled_killswitch_rejects_all_voies() {
    let (store, _tmp) = temp_store().await;
    let gate = EmissionGate::new(Default::default());

    // Sanity : kill-switch désarmé (défaut mint_enabled=true) → réservation OK.
    let ok = gate
        .reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), Voie::OnRamp, Some(dec("0.01")))
        .await
        .expect("default mint_enabled=true → reserve passes");
    println!("T11 baseline (mint_enabled=true) reserve(0.01) → amount={}", ok.amount);
    assert_eq!(ok.amount, dec("0.01"));

    // Arme le kill-switch via le chemin gouverné (ConfigUpdate), pas un write direct.
    store
        .apply_config_update(&ConfigUpdate::SetMintEnabled { enabled: false }, "killswitch-block", 1)
        .unwrap();
    assert!(!store.get_runtime_config().unwrap().mint_enabled, "kill-switch armé");

    // TOUTE voie est désormais refusée par MintDisabled (chokepoint unique).
    for voie in [Voie::OnRamp, Voie::Baseline, Voie::TokenConversion, Voie::Faucet] {
        let label = voie.as_str();
        let res = gate
            .reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), voie, Some(dec("0.001")))
            .await;
        println!("T11 mint_enabled=false reserve({label}) → {res:?}");
        assert!(
            matches!(res, Err(EmissionError::MintDisabled)),
            "G6: kill-switch DOIT refuser la voie {label} (MintDisabled), got {res:?}"
        );
    }

    // Aucune réservation pendant le halt : le compteur n'a pas bougé (seul le
    // 0.01 d'avant le halt est compté).
    let snap = gate.snapshot().await;
    println!("T11 after kill-switch: emitted={}", snap.emitted);
    assert_eq!(snap.emitted, dec("0.01"), "le kill-switch ne réserve rien");

    // Réactivation (gouvernance) → le mint repasse.
    store
        .apply_config_update(&ConfigUpdate::SetMintEnabled { enabled: true }, "reenable-block", 2)
        .unwrap();
    let ok2 = gate
        .reserve(&store, NOW_EPOCH_100, dec("1000"), std_params(), Voie::OnRamp, Some(dec("0.001")))
        .await
        .expect("re-enabled → reserve passes");
    println!("T11 re-enabled reserve(0.001) → amount={}", ok2.amount);
    assert_eq!(ok2.amount, dec("0.001"));

    println!("\n   T11/G6 PASSED: mint_enabled=false refuse toutes les voies (MintDisabled), réactivation OK.");
}

/// T12 / G9 — le couloir d'émission est gouverné : un `SetEmissionCorridor`
/// change le budget de la période.
///
/// Le couloir vit dans `RuntimeConfig` (modifiable par gouvernance timelock) ;
/// `params_from_runtime` le lit (fallback boot). On prouve la boucle complète :
/// `ConfigUpdate::SetEmissionCorridor` → `RuntimeConfig` → `params_from_runtime`
/// → budget de `EmissionGate::reserve`. Une baisse du plafond réduit le budget à
/// l'epoch suivant.
#[tokio::test]
async fn t12_emission_corridor_governs_budget() {
    let (store, _tmp) = temp_store().await;
    let settings = pms_config::load_config().expect("load_config");
    let gate = EmissionGate::new(Default::default());

    // 1) Gouvernance fixe le couloir à 20 %/an (ceiling=target=2000 bps).
    store
        .apply_config_update(
            &ConfigUpdate::SetEmissionCorridor {
                ceiling_bps: 2000,
                floor_bps: 0,
                target_bps: 2000,
                epoch_duration_sec: 86_400,
            },
            "gov-corridor-20pct",
            1,
        )
        .unwrap();
    let rt = store.get_runtime_config().unwrap();
    let params_20 = pms_server::emission_mint::params_from_runtime(&settings, &rt);
    println!("corridor governed: ceiling_pct={}, target_pct={}", params_20.ceiling_pct, params_20.target_pct);
    assert_eq!(params_20.ceiling_pct, 20.0, "bps→pct: 2000 bps = 20%");
    assert_eq!(params_20.target_pct, 20.0);

    // Epoch 100, supply 1000 → baseline réserve le budget = 1000 × 20% / 365.
    let r20 = gate
        .reserve(&store, NOW_EPOCH_100, dec("1000"), params_20, Voie::Baseline, None)
        .await
        .unwrap();
    println!("epoch100 budget @20% → {}", r20.amount);
    // 1000 * 0.20 / 365 = 0.547945205...
    assert!(r20.amount > dec("0.5479") && r20.amount < dec("0.5480"), "budget@20% ~= 0.54794, got {}", r20.amount);

    // 2) Gouvernance BAISSE le couloir à 5 % (resserrage).
    store
        .apply_config_update(
            &ConfigUpdate::SetEmissionCorridor {
                ceiling_bps: 500,
                floor_bps: 0,
                target_bps: 500,
                epoch_duration_sec: 86_400,
            },
            "gov-corridor-5pct",
            2,
        )
        .unwrap();
    let rt2 = store.get_runtime_config().unwrap();
    let params_5 = pms_server::emission_mint::params_from_runtime(&settings, &rt2);
    assert_eq!(params_5.ceiling_pct, 5.0, "bps→pct: 500 bps = 5%");

    // Epoch 101 (rollover) → nouveau budget = 1000 × 5% / 365.
    let r5 = gate
        .reserve(&store, NOW_EPOCH_101, dec("1000"), params_5, Voie::Baseline, None)
        .await
        .unwrap();
    println!("epoch101 budget @5% → {}", r5.amount);
    // 1000 * 0.05 / 365 = 0.136986301...
    assert!(r5.amount > dec("0.1369") && r5.amount < dec("0.1370"), "budget@5% ~= 0.13698, got {}", r5.amount);
    assert!(r5.amount < r20.amount, "G9: baisser le couloir RÉDUIT le budget ({} < {})", r5.amount, r20.amount);

    println!("\n   T12/G9 PASSED: le couloir d'émission est gouverné (20% → 5% via SetEmissionCorridor change le budget).");
}
