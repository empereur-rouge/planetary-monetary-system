// crates/pms-core/tests/spend_authorization.rs
//
// Tests d'attaque pour `validate_transaction_full` (audit C-1 + C-2 + M-7).
//
// Le scénario central de l'audit : un attaquant construit une transaction qui
// dépense l'UTXO d'une victime, signe avec SA PROPRE clé (signature ECDSA
// parfaitement valide), et soumet. Avant le fix, rien ne liait la pubkey de
// l'unlock à l'adresse propriétaire de l'UTXO → vol de fonds.
//
// Exécution :
//   cargo test --release -p pms-core --test spend_authorization -- --nocapture

use pms_core::utxo::ShardedUtxoSet;
use pms_core::validations::check::ValidatePolicy;
use pms_core::validations::transactions::validate_transaction_full;
use pms_types::{OutputId, Transaction, TxInput, TxOutput, Unlock};
use pms_wallet::{SignerBackend, Wallet};

const NETWORK_ID: &str = "pms-testnet-v1";

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

fn output(address: &str, amount: &str, asset_id: Option<&str>) -> TxOutput {
    TxOutput::new(address, amount, asset_id.map(Into::into))
}

/// Signe `tx` avec `wallet` et remplit un unlock PAR input (appariement
/// positionnel, comme le SDK et les handlers serveur).
fn sign_tx(wallet: &Wallet, tx: &Transaction, network_id: &str) -> Transaction {
    let msg_hex = tx.signing_message(network_id).expect("signing_message");
    let sig = wallet.sign(&msg_hex).expect("sign");
    Transaction {
        inputs: tx.inputs.clone(),
        outputs: tx.outputs.clone(),
        fee: tx.fee.clone(),
        unlocks: tx
            .inputs
            .iter()
            .map(|_| Unlock {
                pubkey_hex: wallet.public_key_hex.clone(),
                signature_b64: sig.clone(),
            })
            .collect(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// C-1 : l'attaque centrale de l'audit — dépenser l'UTXO d'autrui
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn attacker_cannot_spend_victim_utxo_with_own_key() {
    let victim = Wallet::from_seed(&[1u8; 32], None).expect("victim wallet");
    let attacker = Wallet::from_seed(&[2u8; 32], None).expect("attacker wallet");
    let victim_addr = victim.get_address("8e");
    let attacker_addr = attacker.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("victim-tx", 0), output(&victim_addr, "100.0", None))
        .await;

    // L'attaquant dépense l'UTXO de la victime vers sa propre adresse,
    // signé avec SA clé — signature ECDSA 100% valide.
    let theft = Transaction {
        inputs: vec![TxInput {
            out: out_id("victim-tx", 0),
        }],
        outputs: vec![output(&attacker_addr, "100.0", None)],
        fee: "0".into(),
        unlocks: vec![],
    };
    let theft_signed = sign_tx(&attacker, &theft, NETWORK_ID);

    let result = validate_transaction_full(&utxos, &theft_signed, &test_policy(), pms_core::utxo::current_time_ms()).await;
    println!("THEFT ATTEMPT result: {result:?}");
    let err = format!("{:?}", result.expect_err("theft MUST be rejected"));
    assert!(
        err.contains("OwnershipMismatch"),
        "expected OwnershipMismatch, got: {err}"
    );
}

#[tokio::test]
async fn legit_owner_spend_is_accepted_bech32m() {
    let owner = Wallet::from_seed(&[3u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");
    let dest = Wallet::from_seed(&[4u8; 32], None)
        .expect("wallet")
        .get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("mint-1", 0), output(&owner_addr, "50.0", None))
        .await;

    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("mint-1", 0),
        }],
        outputs: vec![
            output(&dest, "40.0", None),
            output(&owner_addr, "10.0", None), // change
        ],
        fee: "0".into(),
        unlocks: vec![],
    };
    let tx_signed = sign_tx(&owner, &tx, NETWORK_ID);

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms()).await;
    println!("LEGIT SPEND (bech32m) result: {result:?}");
    let fetched = result.expect("legit owner spend must be accepted");
    println!(
        "fetched input outputs: {:?}",
        fetched.iter().map(|o| &o.address).collect::<Vec<_>>()
    );
    assert_eq!(fetched.len(), 1);
}

#[tokio::test]
async fn legit_owner_spend_is_accepted_raw_pubkey_address() {
    // Forme d'adresse SDK : l'adresse de l'UTXO est la pubkey hex brute.
    let owner = Wallet::from_seed(&[5u8; 32], None).expect("wallet");
    let owner_addr = owner.public_key_hex.clone();

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("mint-2", 0), output(&owner_addr, "10.0", None))
        .await;

    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("mint-2", 0),
        }],
        outputs: vec![output(&owner_addr, "10.0", None)],
        fee: "0".into(),
        unlocks: vec![],
    };
    let tx_signed = sign_tx(&owner, &tx, NETWORK_ID);

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms()).await;
    println!("LEGIT SPEND (raw pubkey addr) result: {result:?}");
    result.expect("raw-pubkey-address owner spend must be accepted");
}

#[tokio::test]
async fn mixed_inputs_one_foreign_utxo_rejected() {
    // L'attaquant possède un vrai UTXO et glisse l'UTXO d'une victime en
    // input[1], avec ses propres unlocks partout. L'input[1] doit être
    // détecté comme non-autorisé.
    let attacker = Wallet::from_seed(&[6u8; 32], None).expect("wallet");
    let victim = Wallet::from_seed(&[7u8; 32], None).expect("wallet");
    let attacker_addr = attacker.get_address("8e");
    let victim_addr = victim.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("atk-own", 0), output(&attacker_addr, "5.0", None))
        .await;
    utxos
        .add(out_id("victim-2", 0), output(&victim_addr, "95.0", None))
        .await;

    let tx = Transaction {
        inputs: vec![
            TxInput {
                out: out_id("atk-own", 0),
            },
            TxInput {
                out: out_id("victim-2", 0),
            },
        ],
        outputs: vec![output(&attacker_addr, "100.0", None)],
        fee: "0".into(),
        unlocks: vec![],
    };
    let tx_signed = sign_tx(&attacker, &tx, NETWORK_ID);

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms()).await;
    println!("MIXED-INPUT THEFT result: {result:?}");
    let err = format!("{:?}", result.expect_err("foreign input must be rejected"));
    assert!(
        err.contains("OwnershipMismatch") && err.contains("input_index: 1"),
        "expected OwnershipMismatch on input 1, got: {err}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// C-2 : signatures de transaction réellement vérifiées
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn missing_unlocks_rejected() {
    let owner = Wallet::from_seed(&[8u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("mint-3", 0), output(&owner_addr, "10.0", None))
        .await;

    // Aucun unlock du tout — la tx "à l'ancienne" qui passait avant le fix.
    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("mint-3", 0),
        }],
        outputs: vec![output(&owner_addr, "10.0", None)],
        fee: "0".into(),
        unlocks: vec![],
    };

    let result = validate_transaction_full(&utxos, &tx, &test_policy(), pms_core::utxo::current_time_ms()).await;
    println!("NO-UNLOCK TX result: {result:?}");
    let err = format!("{:?}", result.expect_err("tx without unlocks must be rejected"));
    assert!(
        err.contains("InvalidSignature"),
        "expected InvalidSignature (pairing), got: {err}"
    );
}

#[tokio::test]
async fn wrong_network_signature_rejected() {
    let owner = Wallet::from_seed(&[9u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("mint-4", 0), output(&owner_addr, "10.0", None))
        .await;

    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("mint-4", 0),
        }],
        outputs: vec![output(&owner_addr, "10.0", None)],
        fee: "0".into(),
        unlocks: vec![],
    };
    // Signée pour mainnet, validée sur testnet → replay cross-chain rejeté.
    let tx_signed = sign_tx(&owner, &tx, "pms-mainnet-v1");

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms()).await;
    println!("CROSS-NETWORK REPLAY result: {result:?}");
    let err = format!("{:?}", result.expect_err("cross-network replay must be rejected"));
    assert!(
        err.contains("InvalidSignature"),
        "expected InvalidSignature, got: {err}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// M-7 : fee sanity + conservation par asset
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn ghost_fee_above_max_rejected() {
    let owner = Wallet::from_seed(&[10u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("mint-5", 0), output(&owner_addr, "10.0", None))
        .await;

    // Conservation OK (10 in == 10 out) mais fee déclaré délirant —
    // avant le fix, le champ fee n'était validé nulle part en production.
    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("mint-5", 0),
        }],
        outputs: vec![output(&owner_addr, "10.0", None)],
        fee: "999999999".into(),
        unlocks: vec![],
    };
    let tx_signed = sign_tx(&owner, &tx, NETWORK_ID);

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms()).await;
    println!("GHOST FEE result: {result:?}");
    let err = format!("{:?}", result.expect_err("ghost fee must be rejected"));
    assert!(err.contains("FeeTooHigh"), "expected FeeTooHigh, got: {err}");
}

#[tokio::test]
async fn malformed_fee_rejected() {
    let owner = Wallet::from_seed(&[11u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("mint-6", 0), output(&owner_addr, "10.0", None))
        .await;

    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("mint-6", 0),
        }],
        outputs: vec![output(&owner_addr, "10.0", None)],
        fee: "not-a-number".into(),
        unlocks: vec![],
    };
    let tx_signed = sign_tx(&owner, &tx, NETWORK_ID);

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms()).await;
    println!("MALFORMED FEE result: {result:?}");
    assert!(result.is_err(), "malformed fee must be rejected");
}

#[tokio::test]
async fn cross_asset_conversion_rejected() {
    // 10 PMS en input ne peuvent pas devenir 10 EDN en output, même si les
    // sommes globales sont égales (conservation PAR ASSET).
    let owner = Wallet::from_seed(&[12u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("mint-7", 0), output(&owner_addr, "10.0", None))
        .await;

    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("mint-7", 0),
        }],
        outputs: vec![output(&owner_addr, "10.0", Some("edenite"))],
        fee: "0".into(),
        unlocks: vec![],
    };
    let tx_signed = sign_tx(&owner, &tx, NETWORK_ID);

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms()).await;
    println!("CROSS-ASSET CONVERSION result: {result:?}");
    let err = format!("{:?}", result.expect_err("cross-asset conversion must be rejected"));
    assert!(
        err.contains("AssetBalanceMismatch"),
        "expected AssetBalanceMismatch, got: {err}"
    );
}
