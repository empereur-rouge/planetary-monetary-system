// crates/pms-core/tests/mint_constraints.rs
//
// Tests du mint contraint per-asset (plan 2.3 / 2.4, v0.10.0) :
// `validate_custom_asset_mints` enforce TokenMetadata au niveau protocole —
// asset enregistré, mint_authority, granularité decimals, supply cap.
//
// Exécution :
//   cargo test --release -p pms-core --test mint_constraints -- --nocapture

use pms_core::validations::mint::{minted_amounts_by_custom_asset, validate_custom_asset_mints};
use pms_types::TokenMetadata as Meta; // alias court pour les helpers locaux

/// Wrapper test : calcule `minted` comme le fait persist.rs puis valide.
fn validate(
    outputs: &[TxOutput],
    signer: &str,
    metadata: &HashMap<String, Option<Meta>>,
    circulating: &HashMap<String, Decimal>,
) -> Result<(), pms_errors::ValidationError> {
    validate_with_collateral(outputs, signer, metadata, circulating, &HashMap::new())
}

/// Variante avec réserve : `locked_collateral` = somme des UTXOs de réserve
/// encore time-lockés, par asset (comme résolue par persist.rs).
fn validate_with_collateral(
    outputs: &[TxOutput],
    signer: &str,
    metadata: &HashMap<String, Option<Meta>>,
    circulating: &HashMap<String, Decimal>,
    locked_collateral: &HashMap<String, Decimal>,
) -> Result<(), pms_errors::ValidationError> {
    let minted = minted_amounts_by_custom_asset(outputs)?;
    validate_custom_asset_mints(outputs, signer, &minted, metadata, circulating, locked_collateral)
}
use pms_types::{TokenMetadata, TxOutput};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;

const AUTHORITY_PK: &str = "04aabbccddeeff00112233445566778899";
const OTHER_PK: &str = "04ffeeddccbbaa99887766554433221100";

fn meta(asset_id: &str, decimals: u8, max_supply: Option<&str>) -> TokenMetadata {
    TokenMetadata {
        asset_id: asset_id.into(),
        symbol: "TST".into(),
        name: "Test Token".into(),
        decimals,
        max_supply: max_supply.map(Into::into),
        creator: "8e1creator".into(),
        mint_authority: AUTHORITY_PK.into(),
        demurrage_bps_per_day: None,
        collateral_address: None,
        collateral_asset_id: None,
        collateral_ratio_bps: None,
        royalty_bps: None,
        royalty_beneficiary: None,
    }
}

/// Metadata avec mint collatéralisé (réserve `8e1reserveaddr`, ratio en bps).
fn meta_collateralized(asset_id: &str, ratio_bps: u32) -> TokenMetadata {
    TokenMetadata {
        collateral_address: Some("8e1reserveaddr".into()),
        collateral_asset_id: None, // collatéral = natif
        collateral_ratio_bps: Some(ratio_bps),
        royalty_bps: None,
        royalty_beneficiary: None,
        ..meta(asset_id, 8, None)
    }
}

fn mint_outputs(asset_id: &str, amounts: &[&str]) -> Vec<TxOutput> {
    amounts
        .iter()
        .map(|a| TxOutput::new("8e1recipient", *a, Some(asset_id.into())))
        .collect()
}

fn metas(entries: &[(&str, Option<TokenMetadata>)]) -> HashMap<String, Option<TokenMetadata>> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn supply(entries: &[(&str, &str)]) -> HashMap<String, Decimal> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), Decimal::from_str(v).unwrap()))
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// Cas malveillants → rejet
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn mint_of_unregistered_asset_keeps_legacy_behavior() {
    // Asset jamais enregistré via TokenCreate → AUCUNE contrainte per-asset
    // (gate Coordinator seul, comportement historique). Indispensable : les
    // refunds de contrats (edenite-cube-burn) mintent des assets non
    // enregistrés — les rejeter casserait le flux burn→refund production.
    let outputs = mint_outputs("ghost-token", &["100"]);
    let result = validate(
        &outputs,
        OTHER_PK, // même un signataire quelconque : pas de metadata, pas de binding
        &metas(&[("ghost-token", None)]), // lookup fait, asset absent du registre
        &supply(&[]),
    );
    println!("UNREGISTERED asset mint (legacy) → {result:?}");
    assert!(result.is_ok(), "unregistered asset must keep legacy behavior: {result:?}");
}

#[test]
fn mint_by_non_authority_rejected() {
    let outputs = mint_outputs("edenite", &["100"]);
    let result = validate(
        &outputs,
        OTHER_PK, // signataire ≠ mint_authority
        &metas(&[("edenite", Some(meta("edenite", 8, None)))]),
        &supply(&[]),
    );
    println!("NON-AUTHORITY mint → {result:?}");
    let err = format!("{:?}", result.expect_err("must reject"));
    assert!(err.contains("UnauthorizedTokenMint"), "got: {err}");
}

#[test]
fn mint_exceeding_max_supply_rejected() {
    // circulating 900 + mint 200 > max 1000
    let outputs = mint_outputs("capped", &["200"]);
    let result = validate(&outputs, AUTHORITY_PK, &metas(&[("capped", Some(meta("capped", 8, Some("1000"))))]), &supply(&[("capped", "900")]));
    println!("OVER-CAP mint (900 + 200 > 1000) → {result:?}");
    let err = format!("{:?}", result.expect_err("must reject"));
    assert!(err.contains("MaxSupplyExceeded"), "got: {err}");
}

#[test]
fn mint_violating_decimals_rejected() {
    // asset decimals=0 → un montant fractionnaire est invalide
    let outputs = mint_outputs("integer-only", &["0.5"]);
    let result = validate(&outputs, AUTHORITY_PK, &metas(&[("integer-only", Some(meta("integer-only", 0, None)))]), &supply(&[]));
    println!("DECIMALS violation (0.5 sur decimals=0) → {result:?}");
    let err = format!("{:?}", result.expect_err("must reject"));
    assert!(err.contains("InvalidAmount"), "got: {err}");
}

#[test]
fn mint_with_negative_or_zero_amount_rejected() {
    for bad in ["0", "-5"] {
        let outputs = mint_outputs("edenite", &[bad]);
        let result = minted_amounts_by_custom_asset(&outputs);
        println!("BAD amount {bad:?} → {result:?}");
        assert!(result.is_err(), "amount {bad} must be rejected");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Cas légitimes → acceptés
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn mint_by_authority_within_cap_accepted() {
    // circulating 900 + mint 100 == max 1000 → exactement à la cap, OK
    let outputs = mint_outputs("capped", &["100"]);
    let result = validate(&outputs, AUTHORITY_PK, &metas(&[("capped", Some(meta("capped", 8, Some("1000"))))]), &supply(&[("capped", "900")]));
    println!("AT-CAP mint (900 + 100 == 1000) → {result:?}");
    assert!(result.is_ok(), "mint at exact cap must pass: {result:?}");
}

#[test]
fn mint_unlimited_supply_accepted() {
    let outputs = mint_outputs("unlimited", &["999999999"]);
    let result = validate(&outputs, AUTHORITY_PK, &metas(&[("unlimited", Some(meta("unlimited", 8, None)))]), &supply(&[("unlimited", "123456789")]));
    println!("UNLIMITED supply mint → {result:?}");
    assert!(result.is_ok(), "no cap = no limit: {result:?}");
}

#[test]
fn mint_authority_comparison_is_case_insensitive() {
    let outputs = mint_outputs("edenite", &["10"]);
    let result = validate(&outputs, &AUTHORITY_PK.to_uppercase(), &metas(&[("edenite", Some(meta("edenite", 8, None)))]), &supply(&[]));
    println!("CASE-INSENSITIVE authority → {result:?}");
    assert!(result.is_ok(), "hex case must not matter: {result:?}");
}

#[test]
fn native_pms_outputs_ignored_by_custom_asset_checks() {
    // Mint 100% natif : aucune métadonnée requise.
    let outputs = vec![TxOutput::new("8e1recipient", "1000", None)];
    let result = validate(&outputs, OTHER_PK, &metas(&[]), &supply(&[]));
    println!("NATIVE-ONLY mint → {result:?}");
    assert!(result.is_ok(), "native outputs are out of scope: {result:?}");
}

#[test]
fn multi_output_amounts_are_summed_per_asset() {
    // 3 outputs de 400 chacun = 1200 > cap 1000, même si chaque output passe seul.
    let outputs = mint_outputs("capped", &["400", "400", "400"]);
    let minted = minted_amounts_by_custom_asset(&outputs).unwrap();
    println!("summed minted: {minted:?}");
    assert_eq!(minted["capped"], Decimal::from(1200));

    let result = validate(&outputs, AUTHORITY_PK, &metas(&[("capped", Some(meta("capped", 8, Some("1000"))))]), &supply(&[]));
    println!("SPLIT over-cap mint (3×400 > 1000) → {result:?}");
    let err = format!("{:?}", result.expect_err("sum must be checked, not per-output"));
    assert!(err.contains("MaxSupplyExceeded"), "got: {err}");
}

// ═══════════════════════════════════════════════════════════════════════════
// Mint collatéralisé (protocole 2.3 v2)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn collateralized_mint_within_locked_reserve_accepted() {
    // Réserve verrouillée 1000, ratio 1:1 → mint 800 avec circulation 0 : OK
    let outputs = mint_outputs("backed", &["800"]);
    let result = validate_with_collateral(
        &outputs,
        AUTHORITY_PK,
        &metas(&[("backed", Some(meta_collateralized("backed", 10_000)))]),
        &supply(&[("backed", "0")]),
        &supply(&[("backed", "1000")]), // locked collateral
    );
    println!("COLLATERAL 800 <= 1000 (1:1) → {result:?}");
    assert!(result.is_ok(), "mint couvert doit passer: {result:?}");
}

#[test]
fn collateralized_mint_exceeding_reserve_rejected() {
    // Réserve 1000, circulation 500, mint 600 → requis 1100 > 1000 : rejet
    let outputs = mint_outputs("backed", &["600"]);
    let result = validate_with_collateral(
        &outputs,
        AUTHORITY_PK,
        &metas(&[("backed", Some(meta_collateralized("backed", 10_000)))]),
        &supply(&[("backed", "500")]),
        &supply(&[("backed", "1000")]),
    );
    println!("COLLATERAL (500+600) > 1000 → {result:?}");
    let err = format!("{:?}", result.expect_err("émission totale > réserve doit être rejetée"));
    assert!(err.contains("InsufficientCollateral"), "got: {err}");
}

#[test]
fn collateralized_mint_at_exact_coverage_accepted() {
    // Réserve 1000, circulation 400, mint 600 → requis 1000 == 1000 : OK
    let outputs = mint_outputs("backed", &["600"]);
    let result = validate_with_collateral(
        &outputs,
        AUTHORITY_PK,
        &metas(&[("backed", Some(meta_collateralized("backed", 10_000)))]),
        &supply(&[("backed", "400")]),
        &supply(&[("backed", "1000")]),
    );
    println!("COLLATERAL (400+600) == 1000 → {result:?}");
    assert!(result.is_ok(), "couverture exacte doit passer: {result:?}");
}

#[test]
fn collateral_ratio_is_applied() {
    // Ratio 150% (15000 bps) : mint 100 exige 150 de réserve. 140 → rejet, 150 → OK.
    let outputs = mint_outputs("over", &["100"]);
    let metas_over = metas(&[("over", Some(meta_collateralized("over", 15_000)))]);

    let rejected = validate_with_collateral(
        &outputs, AUTHORITY_PK, &metas_over, &supply(&[]), &supply(&[("over", "140")]),
    );
    println!("RATIO 150%: locked 140 < 150 → {rejected:?}");
    assert!(format!("{rejected:?}").contains("InsufficientCollateral"));

    let accepted = validate_with_collateral(
        &outputs, AUTHORITY_PK, &metas_over, &supply(&[]), &supply(&[("over", "150")]),
    );
    println!("RATIO 150%: locked 150 == 150 → {accepted:?}");
    assert!(accepted.is_ok(), "{accepted:?}");
}

#[test]
fn no_locked_collateral_blocks_any_mint() {
    // Réserve vide (ou tous les locks expirés → somme 0) : aucun mint possible.
    let outputs = mint_outputs("backed", &["1"]);
    let result = validate_with_collateral(
        &outputs,
        AUTHORITY_PK,
        &metas(&[("backed", Some(meta_collateralized("backed", 10_000)))]),
        &supply(&[]),
        &supply(&[]), // pas d'entrée = 0 verrouillé
    );
    println!("NO LOCKED collateral → {result:?}");
    assert!(format!("{:?}", result.expect_err("must reject")).contains("InsufficientCollateral"));
}

#[test]
fn sum_locked_collateral_filters_unlocked_expired_and_wrong_asset() {
    use pms_core::validations::mint::sum_locked_collateral;
    use pms_types::OutputId;
    let now: u64 = 1_000_000;
    let oid = |i: u32| OutputId { txid: "rsv".into(), index: i };

    let utxos = vec![
        // compte : natif, lock futur
        (oid(0), TxOutput::new_locked("8e1reserveaddr", "100", None, now + 1)),
        // ne compte pas : lock EXPIRÉ (l'émetteur peut retirer)
        (oid(1), TxOutput::new_locked("8e1reserveaddr", "50", None, now)),
        // ne compte pas : pas de lock du tout
        (oid(2), TxOutput::new("8e1reserveaddr", "25", None)),
        // ne compte pas : mauvais asset
        (oid(3), TxOutput::new_locked("8e1reserveaddr", "999", Some("other".into()), now + 1)),
    ];
    let locked = sum_locked_collateral(&utxos, &None, now);
    println!("sum_locked_collateral: {locked} (attendu 100 : exclut expiré/sans-lock/autre asset)");
    assert_eq!(locked, Decimal::from(100));

    // ciblage d'un asset de collatéral spécifique
    let locked_other = sum_locked_collateral(&utxos, &Some("other".into()), now);
    println!("sum pour asset 'other': {locked_other}");
    assert_eq!(locked_other, Decimal::from(999));
}
