// crates/pms-core/tests/demurrage_validation.rs
//
// Tests du demurrage opt-in par asset (protocole 2.5, v0.10.0) à travers
// `validate_transaction_full` : conservation `out <= effective_in` pour les
// assets à décote, conservation STRICTE inchangée pour les autres.
//
// Exécution :
//   cargo test --release -p pms-core --test demurrage_validation -- --nocapture

use pms_core::utxo::ShardedUtxoSet;
use pms_core::validations::check::ValidatePolicy;
use pms_core::validations::demurrage::{DAY_MS, effective_value};
use pms_core::validations::transactions::validate_transaction_full;
use pms_types::{OutputId, Transaction, TxInput, TxOutput, Unlock};
use pms_wallet::{SignerBackend, Wallet};
use rust_decimal::Decimal;
use std::collections::HashMap;

const NETWORK_ID: &str = "pms-testnet-v1";
const ASSET: &str = "melting";
const BPS_PER_DAY: u32 = 100; // 1 % / jour

fn test_policy() -> ValidatePolicy {
    let mut p = ValidatePolicy::default();
    p.network_id = NETWORK_ID.to_string();
    p
}

fn out_id(txid: &str, index: u32) -> OutputId {
    OutputId {
        txid: txid.into(),
        index,
    }
}

fn rates() -> HashMap<String, u32> {
    HashMap::from([(ASSET.to_string(), BPS_PER_DAY)])
}

fn sign_tx(wallet: &Wallet, tx: &Transaction) -> Transaction {
    let msg = tx.signing_message(NETWORK_ID).expect("signing_message");
    let sig = wallet.sign(&msg).expect("sign");
    Transaction {
        inputs: tx.inputs.clone(),
        outputs: tx.outputs.clone(),
        fee: tx.fee.clone(),
        unlocks: tx
            .inputs
            .iter()
            .map(|_| Unlock::new(wallet.public_key_hex.clone(), sig.clone()))
            .collect(),
    }
}

/// Sandbox : un UTXO de 1000 MELTING créé à t=created_at pour `owner`.
async fn setup(owner: &Wallet, created_at: u64) -> ShardedUtxoSet {
    let utxos = ShardedUtxoSet::new(0, None);
    let mut out = TxOutput::new(owner.get_address("8e"), "1000", Some(ASSET.into()));
    out.created_at = Some(created_at);
    utxos.add(out_id("melt-fund", 0), out).await;
    utxos
}

fn spend(owner: &Wallet, dest: &str, amount: &str) -> Transaction {
    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("melt-fund", 0),
        }],
        outputs: vec![TxOutput::new(dest, amount, Some(ASSET.into()))],
        fee: "0".into(),
        unlocks: vec![],
    };
    sign_tx(owner, &tx)
}

// ═══════════════════════════════════════════════════════════════════════════
// Cas malveillant : dépenser la valeur NOMINALE après décote → rejet
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn spending_nominal_value_after_decay_rejected() {
    let owner = Wallet::from_seed(&[60u8; 32], None).expect("wallet");
    let dest = Wallet::from_seed(&[61u8; 32], None)
        .expect("wallet")
        .get_address("8e");

    let created = 1_000_000;
    let now = created + 3 * DAY_MS; // 3 jours pleins → décote 3 %
    let utxos = setup(&owner, created).await;

    // L'utilisateur tente de dépenser les 1000 nominaux (effective = 970)
    let tx = spend(&owner, &dest, "1000");
    let result = validate_transaction_full(&utxos, &tx, &test_policy(), now, &rates()).await;
    println!("NOMINAL spend after 3d decay (1000 > 970) → {result:?}");
    let err = format!("{:?}", result.expect_err("nominal > effective DOIT être rejeté"));
    assert!(err.contains("AssetBalanceMismatch"), "got: {err}");
    assert!(err.contains("970"), "inputs(effective)=970 attendu: {err}");
}

// ═══════════════════════════════════════════════════════════════════════════
// Cas légitime : dépenser la valeur EFFECTIVE → accepté
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn spending_effective_value_after_decay_accepted() {
    let owner = Wallet::from_seed(&[62u8; 32], None).expect("wallet");
    let dest = Wallet::from_seed(&[63u8; 32], None)
        .expect("wallet")
        .get_address("8e");

    let created = 1_000_000;
    let now = created + 3 * DAY_MS;
    let utxos = setup(&owner, created).await;

    // valeur effective attendue : 1000 - 1000×100×3/10000 = 970
    let expected = effective_value(Decimal::from(1000), Some(created), now, BPS_PER_DAY);
    println!("effective après 3 jours à 1%/j : {expected}");
    assert_eq!(expected, Decimal::from(970));

    let tx = spend(&owner, &dest, "970");
    let result = validate_transaction_full(&utxos, &tx, &test_policy(), now, &rates()).await;
    println!("EFFECTIVE spend (970 == 970) → {result:?}");
    assert!(result.is_ok(), "dépense de la valeur effective doit passer: {result:?}");
}

#[tokio::test]
async fn underspend_burns_extra_for_demurrage_asset() {
    // out < effective_in est PERMIS pour un asset à demurrage (burn volontaire
    // en plus de la décote) — c'est la sémantique « <= ».
    let owner = Wallet::from_seed(&[64u8; 32], None).expect("wallet");
    let dest = owner.get_address("8e");

    let created = 1_000_000;
    let now = created + DAY_MS;
    let utxos = setup(&owner, created).await;

    let tx = spend(&owner, &dest, "500");
    let result = validate_transaction_full(&utxos, &tx, &test_policy(), now, &rates()).await;
    println!("UNDER-spend (500 <= 990) → {result:?}");
    assert!(result.is_ok(), "out <= effective doit passer: {result:?}");
}

#[tokio::test]
async fn same_day_spend_has_no_decay() {
    let owner = Wallet::from_seed(&[65u8; 32], None).expect("wallet");
    let dest = owner.get_address("8e");

    let created = 1_000_000;
    let now = created + DAY_MS - 1; // 23h59m59.999s → 0 jour plein
    let utxos = setup(&owner, created).await;

    let tx = spend(&owner, &dest, "1000");
    let result = validate_transaction_full(&utxos, &tx, &test_policy(), now, &rates()).await;
    println!("SAME-DAY spend (1000 == 1000, 0 jour plein) → {result:?}");
    assert!(result.is_ok(), "pas de décote avant 1 jour plein: {result:?}");
}

#[tokio::test]
async fn pre_upgrade_utxo_without_created_at_does_not_decay() {
    let owner = Wallet::from_seed(&[66u8; 32], None).expect("wallet");
    let dest = owner.get_address("8e");

    // UTXO legacy : created_at = None (écrit avant v0.10.0)
    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(
            out_id("melt-fund", 0),
            TxOutput::new(owner.get_address("8e"), "1000", Some(ASSET.into())),
        )
        .await;

    let tx = spend(&owner, &dest, "1000");
    let now = 100 * DAY_MS; // peu importe : pas de point de départ
    let result = validate_transaction_full(&utxos, &tx, &test_policy(), now, &rates()).await;
    println!("LEGACY UTXO (created_at=None) full spend → {result:?}");
    assert!(result.is_ok(), "UTXO pré-upgrade ne décote pas: {result:?}");
}

#[tokio::test]
async fn strict_conservation_unchanged_for_assets_without_demurrage() {
    let owner = Wallet::from_seed(&[67u8; 32], None).expect("wallet");
    let dest = owner.get_address("8e");

    let created = 1_000_000;
    let now = created + 10 * DAY_MS;
    let utxos = setup(&owner, created).await;

    // Même UTXO, mais l'asset n'est PAS dans la map demurrage → règle stricte :
    // 1000 == 1000 passe, 999 (under-spend implicite) est REJETÉ (M-7).
    let empty: HashMap<String, u32> = HashMap::new();

    let full = spend(&owner, &dest, "1000");
    let r1 = validate_transaction_full(&utxos, &full, &test_policy(), now, &empty).await;
    println!("NO-DEMURRAGE strict full spend → {r1:?}");
    assert!(r1.is_ok());

    let under = spend(&owner, &dest, "999");
    let r2 = validate_transaction_full(&utxos, &under, &test_policy(), now, &empty).await;
    println!("NO-DEMURRAGE under-spend (implicit burn) → {r2:?}");
    let err = format!("{:?}", r2.expect_err("M-7 strict doit rejeter"));
    assert!(err.contains("AssetBalanceMismatch"), "got: {err}");
}

#[tokio::test]
async fn created_at_survives_cache_roundtrip() {
    let utxos = ShardedUtxoSet::new(0, None);
    let mut out = TxOutput::new("8e1someaddr", "10", Some(ASSET.into()));
    out.created_at = Some(123_456_789);
    utxos.add(out_id("cat-rt", 0), out).await;

    let fetched = utxos.get(&out_id("cat-rt", 0)).await.expect("utxo");
    println!("cache round-trip created_at: {:?}", fetched.created_at);
    assert_eq!(fetched.created_at, Some(123_456_789), "created_at lost in cache!");
}
