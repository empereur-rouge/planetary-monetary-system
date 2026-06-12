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
            .map(|_| Unlock::new(wallet.public_key_hex.clone(), sig.clone()))
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

    let result = validate_transaction_full(&utxos, &theft_signed, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
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

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
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

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
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

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
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

    let result = validate_transaction_full(&utxos, &tx, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
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

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
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

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
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

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
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

    let result = validate_transaction_full(&utxos, &tx_signed, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
    println!("CROSS-ASSET CONVERSION result: {result:?}");
    let err = format!("{:?}", result.expect_err("cross-asset conversion must be rejected"));
    assert!(
        err.contains("AssetBalanceMismatch"),
        "expected AssetBalanceMismatch, got: {err}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Edge-cases de sécurité (audit invariants S1/S2 — v0.9.9)
// Chaque test est une ATTAQUE qui DOIT être rejetée proprement (sans panic).
// Ils verrouillent contre la régression les protections déjà en place dans
// `validate_transaction_full`.
// ═══════════════════════════════════════════════════════════════════════════

/// S1.3 — `unlocks.len() != inputs.len()` : rejet PROPRE (pas de panic / pas
/// d'index out-of-bounds). Ici 2 inputs mais 1 seul unlock fourni.
#[tokio::test]
async fn unlock_count_mismatch_rejected_no_panic() {
    let owner = Wallet::from_seed(&[20u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos.add(out_id("m20", 0), output(&owner_addr, "10.0", None)).await;
    utxos.add(out_id("m20", 1), output(&owner_addr, "10.0", None)).await;

    let tx = Transaction {
        inputs: vec![
            TxInput { out: out_id("m20", 0) },
            TxInput { out: out_id("m20", 1) },
        ],
        outputs: vec![output(&owner_addr, "20.0", None)],
        fee: "0".into(),
        unlocks: vec![],
    };
    let mut signed = sign_tx(&owner, &tx, NETWORK_ID); // 2 unlocks
    signed.unlocks.truncate(1); // ← 2 inputs, 1 unlock

    let result = validate_transaction_full(&utxos, &signed, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
    println!("COUNT MISMATCH result: {result:?}");
    assert!(result.is_err(), "unlocks/inputs count mismatch must be rejected");
}

/// S1.7 — payload modifié APRÈS signature : changer le montant d'un output
/// invalide la signature (le message canonique couvre les outputs).
#[tokio::test]
async fn tampered_output_amount_after_signing_rejected() {
    let owner = Wallet::from_seed(&[21u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");
    let dest = Wallet::from_seed(&[22u8; 32], None).unwrap().get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos.add(out_id("m21", 0), output(&owner_addr, "100.0", None)).await;

    let tx = Transaction {
        inputs: vec![TxInput { out: out_id("m21", 0) }],
        outputs: vec![output(&dest, "100.0", None)],
        fee: "0".into(),
        unlocks: vec![],
    };
    let mut signed = sign_tx(&owner, &tx, NETWORK_ID);
    // Attaque MITM : après signature, l'attaquant gonfle le montant.
    signed.outputs[0].amount = "999.0".into();

    let result = validate_transaction_full(&utxos, &signed, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
    println!("TAMPERED OUTPUT result: {result:?}");
    assert!(result.is_err(), "post-signature output tampering must be rejected");
}

/// S1.8 — signature vide dans l'unlock : rejet propre (pas de panic au décodage).
#[tokio::test]
async fn empty_signature_unlock_rejected_no_panic() {
    let owner = Wallet::from_seed(&[23u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos.add(out_id("m23", 0), output(&owner_addr, "10.0", None)).await;

    let tx = Transaction {
        inputs: vec![TxInput { out: out_id("m23", 0) }],
        outputs: vec![output(&owner_addr, "10.0", None)],
        fee: "0".into(),
        unlocks: vec![Unlock::new(owner.public_key_hex.clone(), String::new())],
    };

    let result = validate_transaction_full(&utxos, &tx, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
    println!("EMPTY SIGNATURE result: {result:?}");
    assert!(result.is_err(), "empty signature must be rejected");
}

/// S1.8 — pubkey malformée dans l'unlock : rejet propre (pas de panic au hex-decode).
#[tokio::test]
async fn malformed_pubkey_unlock_rejected_no_panic() {
    let owner = Wallet::from_seed(&[24u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos.add(out_id("m24", 0), output(&owner_addr, "10.0", None)).await;

    let tx = Transaction {
        inputs: vec![TxInput { out: out_id("m24", 0) }],
        outputs: vec![output(&owner_addr, "10.0", None)],
        fee: "0".into(),
        unlocks: vec![Unlock::new("not-hex-pubkey-zzz".to_string(), "AAAA".to_string())],
    };

    let result = validate_transaction_full(&utxos, &tx, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
    println!("MALFORMED PUBKEY result: {result:?}");
    assert!(result.is_err(), "malformed pubkey must be rejected");
}

/// S2 — montant négatif en output : rejet (pas de création de valeur via signe).
#[tokio::test]
async fn negative_output_amount_rejected() {
    let owner = Wallet::from_seed(&[25u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");
    let dest = Wallet::from_seed(&[26u8; 32], None).unwrap().get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos.add(out_id("m25", 0), output(&owner_addr, "10.0", None)).await;

    // input 10 = output 15 + output -5 (somme = 10, mais un output NÉGATIF).
    let tx = Transaction {
        inputs: vec![TxInput { out: out_id("m25", 0) }],
        outputs: vec![
            output(&dest, "15.0", None),
            output(&owner_addr, "-5.0", None),
        ],
        fee: "0".into(),
        unlocks: vec![],
    };
    let signed = sign_tx(&owner, &tx, NETWORK_ID);

    let result = validate_transaction_full(&utxos, &signed, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
    println!("NEGATIVE OUTPUT result: {result:?}");
    assert!(result.is_err(), "negative output amount must be rejected");
}

/// S2 — somme des outputs qui DÉBORDE `Decimal` : rejet PROPRE (jamais de panic
/// d'overflow → DoS). Deux outputs proches de `Decimal::MAX`.
#[tokio::test]
async fn output_sum_overflow_rejected_no_panic() {
    let owner = Wallet::from_seed(&[27u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos.add(out_id("m27", 0), output(&owner_addr, "1.0", None)).await;

    // 2 × ~7.9e28 ≈ 1.58e29 > Decimal::MAX (≈7.92e28) → la somme déborde.
    let huge = "79000000000000000000000000000";
    let tx = Transaction {
        inputs: vec![TxInput { out: out_id("m27", 0) }],
        outputs: vec![
            output(&owner_addr, huge, None),
            output(&owner_addr, huge, None),
        ],
        fee: "0".into(),
        unlocks: vec![],
    };
    let signed = sign_tx(&owner, &tx, NETWORK_ID);

    // Doit retourner Err (déséquilibré/overflow), JAMAIS paniquer.
    let result = validate_transaction_full(&utxos, &signed, &test_policy(), pms_core::utxo::current_time_ms(), &Default::default()).await;
    println!("OUTPUT OVERFLOW result: {result:?}");
    assert!(result.is_err(), "overflowing output sum must be rejected cleanly (no panic)");
}
