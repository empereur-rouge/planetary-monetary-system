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
    let minted = minted_amounts_by_custom_asset(outputs)?;
    validate_custom_asset_mints(outputs, signer, &minted, metadata, circulating)
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
