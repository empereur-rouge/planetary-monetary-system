//! Phase 2a — conservation native PMS « frais brûlé à la source » (`out ≤ in`).
//!
//! Le frais de gas PMS n'est plus un output vers le coordinateur : il est la
//! différence `in − out`, **détruite** au niveau de la transaction (modèle UTXO
//! standard). Ces tests verrouillent la nouvelle règle de conservation
//! (production : `check_asset_conservation`) :
//!   - PMS natif : `out ≤ in` accepté (la différence = frais brûlé) ;
//!   - PMS natif : `out > in` toujours REJETÉ (pas de création de monnaie) ;
//!   - old-style (frais en output, `out == in`) toujours accepté (backward-compat) ;
//!   - token custom : égalité stricte conservée (le gas se paie en PMS).
//!
//! Run: `cargo test -p pms-core --test fee_burn_conservation -- --nocapture`

use pms_core::validations::transactions::check_asset_conservation;
use pms_types::{OutputId, Transaction, TxInput, TxOutput};

/// Build a 1-input transaction with the given outputs and declared fee.
/// `check_asset_conservation` only sums `input_outputs` (the resolved inputs)
/// and `tx.outputs`, so the input outpoint content is irrelevant here.
fn tx(outputs: Vec<TxOutput>, fee: &str) -> Transaction {
    Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: "t".into(),
                index: 0,
            },
        }],
        outputs,
        fee: fee.into(),
        unlocks: vec![],
    }
}

#[test]
fn native_pms_fee_burned_at_source_is_accepted() {
    let input_outputs = vec![TxOutput::new("A", "10.0", None)];
    let t = tx(vec![TxOutput::new("B", "9.0", None)], "1.0"); // burn = in − out = 1
    let r = check_asset_conservation(&t, &input_outputs);
    println!("[FEE-BURN] native in=10 out=9 (burn 1) → {r:?}");
    assert!(
        r.is_ok(),
        "native PMS out<=in (gas fee burned at source) must be accepted, got {r:?}"
    );
}

#[test]
fn native_pms_creating_value_is_rejected() {
    let input_outputs = vec![TxOutput::new("A", "10.0", None)];
    let t = tx(vec![TxOutput::new("B", "11.0", None)], "0"); // out > in
    let r = check_asset_conservation(&t, &input_outputs);
    println!("[FEE-BURN] native in=10 out=11 → {r:?}");
    assert!(
        r.is_err(),
        "native PMS out>in (creating value) MUST be rejected — no inflation"
    );
}

#[test]
fn native_pms_exact_conservation_still_accepted() {
    // Old style: explicit fee output to the coordinator → out == in.
    let input_outputs = vec![TxOutput::new("A", "10.0", None)];
    let t = tx(
        vec![
            TxOutput::new("B", "9.0", None),
            TxOutput::new("coordinator", "1.0", None),
        ],
        "1.0",
    );
    let r = check_asset_conservation(&t, &input_outputs);
    println!("[FEE-BURN] native in=10 out=10 (explicit fee output) → {r:?}");
    assert!(
        r.is_ok(),
        "legacy fee-output tx (out==in) must stay valid (backward-compat), got {r:?}"
    );
}

#[test]
fn custom_token_stays_strict() {
    // Gas is paid in PMS, never in the token → custom tokens keep strict ==.
    let input_outputs = vec![TxOutput::new("A", "10.0", Some("edenite".into()))];
    let t = tx(vec![TxOutput::new("B", "9.0", Some("edenite".into()))], "0");
    let r = check_asset_conservation(&t, &input_outputs);
    println!("[FEE-BURN] custom edenite in=10 out=9 → {r:?}");
    assert!(
        r.is_err(),
        "custom token MUST keep strict conservation (in==out), got {r:?}"
    );
}
