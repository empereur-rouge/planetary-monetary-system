// crates/pms-core/tests/multi_token_test.rs
//
// Tests multi-token : validation des transactions multi-asset,
// conservation des balances par asset, requêtes supply/balance par asset,
// et scénarios de mint/transfer de tokens custom.

use pms_core::utxo::ShardedUtxoSet;
use pms_core::validations::transactions::validate_transaction_async;
use pms_types::{OutputId, Transaction, TxInput, TxOutput};
use rust_decimal::Decimal;
use std::str::FromStr;

// ═══════════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════════

fn out_id(txid: &str, index: u32) -> OutputId {
    OutputId {
        txid: txid.into(),
        index,
    }
}

fn pms_output(address: &str, amount: &str) -> TxOutput {
    TxOutput {
        address: address.into(),
        amount: amount.into(),
        asset_id: None,
    }
}

fn token_output(address: &str, amount: &str, asset_id: &str) -> TxOutput {
    TxOutput {
        address: address.into(),
        amount: amount.into(),
        asset_id: Some(asset_id.into()),
    }
}

/// Seed un UTXO PMS dans le ShardedUtxoSet.
async fn seed_pms(utxos: &ShardedUtxoSet, txid: &str, index: u32, address: &str, amount: &str) {
    utxos
        .add(out_id(txid, index), pms_output(address, amount))
        .await;
}

/// Seed un UTXO token custom dans le ShardedUtxoSet.
async fn seed_token(
    utxos: &ShardedUtxoSet,
    txid: &str,
    index: u32,
    address: &str,
    amount: &str,
    asset_id: &str,
) {
    utxos
        .add(out_id(txid, index), token_output(address, amount, asset_id))
        .await;
}

// ═══════════════════════════════════════════════════════════════════════════════
// validate_transaction_async — PMS natif (rétrocompatibilité)
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn valid_pms_only_transaction() {
    let utxos = ShardedUtxoSet::new();
    seed_pms(&utxos, "aa01", 0, "Alice", "10.00000000").await;

    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("aa01", 0),
        }],
        outputs: vec![
            pms_output("Bob", "7.00000000"),
            pms_output("Alice", "3.00000000"), // change
        ],
        fee: "0".into(),
        unlocks: vec![],
    };

    let result = validate_transaction_async(&utxos, &tx).await;
    assert!(result.is_ok(), "valid PMS tx should pass: {result:?}");
}

#[tokio::test]
async fn pms_transaction_unbalanced_rejected() {
    let utxos = ShardedUtxoSet::new();
    seed_pms(&utxos, "aa02", 0, "Alice", "10.00000000").await;

    // outputs > inputs
    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("aa02", 0),
        }],
        outputs: vec![pms_output("Bob", "11.00000000")],
        fee: "0".into(),
        unlocks: vec![],
    };

    let result = validate_transaction_async(&utxos, &tx).await;
    assert!(result.is_err(), "unbalanced PMS tx should fail");
    let err = format!("{:?}", result.unwrap_err());
    assert!(
        err.contains("AssetBalanceMismatch"),
        "error should be AssetBalanceMismatch, got: {err}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// validate_transaction_async — Token custom (Edenite)
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn valid_edenite_transfer() {
    let utxos = ShardedUtxoSet::new();

    // Alice a 100 EDEN + 1 PMS (pour les fees)
    seed_token(&utxos, "bb01", 0, "Alice", "100.00000000", "edenite").await;
    seed_pms(&utxos, "bb02", 0, "Alice", "1.00000000").await;

    // Transfer 60 EDEN à Bob, 40 change à Alice + PMS conservation
    let tx = Transaction {
        inputs: vec![
            TxInput {
                out: out_id("bb01", 0),
            },
            TxInput {
                out: out_id("bb02", 0),
            },
        ],
        outputs: vec![
            token_output("Bob", "60.00000000", "edenite"),
            token_output("Alice", "40.00000000", "edenite"), // change EDEN
            pms_output("FeeRecipient", "0.10000000"),        // fee PMS
            pms_output("Alice", "0.90000000"),                // change PMS
        ],
        fee: "0".into(),
        unlocks: vec![],
    };

    let result = validate_transaction_async(&utxos, &tx).await;
    assert!(result.is_ok(), "valid EDEN transfer should pass: {result:?}");
}

#[tokio::test]
async fn edenite_transfer_unbalanced_rejected() {
    let utxos = ShardedUtxoSet::new();
    seed_token(&utxos, "cc01", 0, "Alice", "50.00000000", "edenite").await;

    // Alice essaie d'envoyer plus d'EDEN qu'elle n'en a
    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("cc01", 0),
        }],
        outputs: vec![token_output("Bob", "60.00000000", "edenite")],
        fee: "0".into(),
        unlocks: vec![],
    };

    let result = validate_transaction_async(&utxos, &tx).await;
    assert!(result.is_err(), "over-spending EDEN should fail");
    let err = format!("{:?}", result.unwrap_err());
    assert!(err.contains("AssetBalanceMismatch"));
}

#[tokio::test]
async fn cannot_create_token_from_nothing() {
    let utxos = ShardedUtxoSet::new();
    // Alice n'a que du PMS
    seed_pms(&utxos, "dd01", 0, "Alice", "10.00000000").await;

    // Tente de créer 100 EDEN à partir de rien
    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("dd01", 0),
        }],
        outputs: vec![
            pms_output("Alice", "10.00000000"),
            token_output("Alice", "100.00000000", "edenite"), // pas d'input EDEN !
        ],
        fee: "0".into(),
        unlocks: vec![],
    };

    let result = validate_transaction_async(&utxos, &tx).await;
    assert!(
        result.is_err(),
        "creating token from nothing should fail"
    );
    let err = format!("{:?}", result.unwrap_err());
    assert!(err.contains("AssetBalanceMismatch"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// validate_transaction_async — Multi-asset dans la même TX
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn valid_multi_asset_transaction() {
    let utxos = ShardedUtxoSet::new();

    // Alice a PMS + EDEN + GOLD
    seed_pms(&utxos, "ee01", 0, "Alice", "5.00000000").await;
    seed_token(&utxos, "ee02", 0, "Alice", "100.00000000", "edenite").await;
    seed_token(&utxos, "ee03", 0, "Alice", "50.0000", "gold").await;

    let tx = Transaction {
        inputs: vec![
            TxInput {
                out: out_id("ee01", 0),
            },
            TxInput {
                out: out_id("ee02", 0),
            },
            TxInput {
                out: out_id("ee03", 0),
            },
        ],
        outputs: vec![
            // PMS: 5 = 4 + 1
            pms_output("Bob", "4.00000000"),
            pms_output("Alice", "1.00000000"),
            // EDEN: 100 = 80 + 20
            token_output("Bob", "80.00000000", "edenite"),
            token_output("Alice", "20.00000000", "edenite"),
            // GOLD: 50 = 30 + 20
            token_output("Charlie", "30.0000", "gold"),
            token_output("Alice", "20.0000", "gold"),
        ],
        fee: "0".into(),
        unlocks: vec![],
    };

    let result = validate_transaction_async(&utxos, &tx).await;
    assert!(
        result.is_ok(),
        "valid multi-asset tx should pass: {result:?}"
    );
}

#[tokio::test]
async fn multi_asset_one_unbalanced_rejected() {
    let utxos = ShardedUtxoSet::new();

    seed_pms(&utxos, "ff01", 0, "Alice", "5.00000000").await;
    seed_token(&utxos, "ff02", 0, "Alice", "100.00000000", "edenite").await;

    // PMS est balanced, mais EDEN ne l'est pas (100 in, 110 out)
    let tx = Transaction {
        inputs: vec![
            TxInput {
                out: out_id("ff01", 0),
            },
            TxInput {
                out: out_id("ff02", 0),
            },
        ],
        outputs: vec![
            pms_output("Alice", "5.00000000"),
            token_output("Bob", "110.00000000", "edenite"), // > 100 input
        ],
        fee: "0".into(),
        unlocks: vec![],
    };

    let result = validate_transaction_async(&utxos, &tx).await;
    assert!(result.is_err(), "EDEN unbalanced should reject entire tx");
}

#[tokio::test]
async fn cross_asset_mixing_rejected() {
    let utxos = ShardedUtxoSet::new();

    // Alice a 100 EDEN
    seed_token(&utxos, "1a01", 0, "Alice", "100.00000000", "edenite").await;

    // Tente de convertir EDEN en GOLD (interdit)
    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("1a01", 0),
        }],
        outputs: vec![token_output("Alice", "100.00000000", "gold")],
        fee: "0".into(),
        unlocks: vec![],
    };

    let result = validate_transaction_async(&utxos, &tx).await;
    assert!(
        result.is_err(),
        "converting EDEN to GOLD should be rejected"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// validate_transaction_async — Double-spend + inputs manquants
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn duplicate_input_in_same_tx_rejected() {
    let utxos = ShardedUtxoSet::new();
    seed_token(&utxos, "2a01", 0, "Alice", "50.00000000", "edenite").await;

    // Même input 2 fois → double-spend
    let tx = Transaction {
        inputs: vec![
            TxInput {
                out: out_id("2a01", 0),
            },
            TxInput {
                out: out_id("2a01", 0),
            },
        ],
        outputs: vec![token_output("Bob", "100.00000000", "edenite")],
        fee: "0".into(),
        unlocks: vec![],
    };

    let result = validate_transaction_async(&utxos, &tx).await;
    assert!(result.is_err());
    let err = format!("{:?}", result.unwrap_err());
    assert!(err.contains("DoubleSpend"));
}

#[tokio::test]
async fn missing_utxo_input_rejected() {
    let utxos = ShardedUtxoSet::new();
    // Pas de seed → input n'existe pas

    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("3a01", 0),
        }],
        outputs: vec![token_output("Bob", "50.00000000", "edenite")],
        fee: "0".into(),
        unlocks: vec![],
    };

    let result = validate_transaction_async(&utxos, &tx).await;
    assert!(result.is_err());
    let err = format!("{:?}", result.unwrap_err());
    assert!(err.contains("MissingInput"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// ShardedUtxoSet — circulating_supply / circulating_supply_by_asset
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn circulating_supply_native_pms_only() {
    let utxos = ShardedUtxoSet::new();

    seed_pms(&utxos, "aa01", 0, "Alice", "100.00000000").await;
    seed_pms(&utxos, "aa02", 0, "Bob", "50.00000000").await;
    seed_token(&utxos, "aa03", 0, "Alice", "9999.00000000", "edenite").await;

    let (supply, count) = utxos.circulating_supply().await;
    assert_eq!(supply, Decimal::from_str("150.00000000").unwrap());
    assert_eq!(count, 2, "only PMS UTXOs counted");
}

#[tokio::test]
async fn circulating_supply_by_asset_edenite() {
    let utxos = ShardedUtxoSet::new();

    seed_pms(&utxos, "bb01", 0, "Alice", "100.00000000").await;
    seed_token(&utxos, "bb02", 0, "Alice", "500.00000000", "edenite").await;
    seed_token(&utxos, "bb03", 0, "Bob", "300.00000000", "edenite").await;
    seed_token(&utxos, "bb04", 0, "Charlie", "200.0000", "gold").await;

    // PMS supply
    let (pms_supply, pms_count) = utxos.circulating_supply_by_asset(None).await;
    assert_eq!(pms_supply, Decimal::from_str("100.00000000").unwrap());
    assert_eq!(pms_count, 1);

    // EDEN supply
    let (eden_supply, eden_count) = utxos.circulating_supply_by_asset(Some("edenite")).await;
    assert_eq!(eden_supply, Decimal::from_str("800.00000000").unwrap());
    assert_eq!(eden_count, 2);

    // GOLD supply
    let (gold_supply, gold_count) = utxos.circulating_supply_by_asset(Some("gold")).await;
    assert_eq!(gold_supply, Decimal::from_str("200.0000").unwrap());
    assert_eq!(gold_count, 1);

    // Unknown token → zero
    let (unknown_supply, unknown_count) = utxos.circulating_supply_by_asset(Some("unknown")).await;
    assert_eq!(unknown_supply, Decimal::ZERO);
    assert_eq!(unknown_count, 0);
}

// ═══════════════════════════════════════════════════════════════════════════════
// ShardedUtxoSet — balance_by_address / balance_by_address_and_asset
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn balance_by_address_pms_retrocompat() {
    let utxos = ShardedUtxoSet::new();

    seed_pms(&utxos, "cc01", 0, "Alice", "10.00000000").await;
    seed_pms(&utxos, "cc02", 0, "Alice", "20.00000000").await;
    seed_token(&utxos, "cc03", 0, "Alice", "999.00000000", "edenite").await;

    // balance_by_address ne retourne que le PMS natif
    let balance = utxos.balance_by_address("Alice").await;
    assert_eq!(balance, Decimal::from_str("30.00000000").unwrap());
}

#[tokio::test]
async fn balance_by_address_and_asset() {
    let utxos = ShardedUtxoSet::new();

    seed_pms(&utxos, "dd01", 0, "Alice", "10.00000000").await;
    seed_token(&utxos, "dd02", 0, "Alice", "500.00000000", "edenite").await;
    seed_token(&utxos, "dd03", 0, "Alice", "200.00000000", "edenite").await;
    seed_token(&utxos, "dd04", 0, "Bob", "100.00000000", "edenite").await;

    // Alice EDEN balance
    let alice_eden = utxos
        .balance_by_address_and_asset("Alice", Some("edenite"))
        .await;
    assert_eq!(alice_eden, Decimal::from_str("700.00000000").unwrap());

    // Bob EDEN balance
    let bob_eden = utxos
        .balance_by_address_and_asset("Bob", Some("edenite"))
        .await;
    assert_eq!(bob_eden, Decimal::from_str("100.00000000").unwrap());

    // Alice PMS balance
    let alice_pms = utxos
        .balance_by_address_and_asset("Alice", None)
        .await;
    assert_eq!(alice_pms, Decimal::from_str("10.00000000").unwrap());

    // Alice GOLD balance (she has none)
    let alice_gold = utxos
        .balance_by_address_and_asset("Alice", Some("gold"))
        .await;
    assert_eq!(alice_gold, Decimal::ZERO);
}

// ═══════════════════════════════════════════════════════════════════════════════
// ShardedUtxoSet — utxos_by_address retourne tous les assets
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn utxos_by_address_all_assets() {
    let utxos = ShardedUtxoSet::new();

    seed_pms(&utxos, "ee01", 0, "Alice", "5.00000000").await;
    seed_token(&utxos, "ee02", 0, "Alice", "100.00000000", "edenite").await;
    seed_token(&utxos, "ee03", 0, "Alice", "50.0000", "gold").await;
    seed_pms(&utxos, "ee04", 0, "Bob", "99.00000000").await;

    let alice_utxos = utxos.utxos_by_address("Alice").await;
    assert_eq!(alice_utxos.len(), 3, "Alice should have 3 UTXOs (PMS + EDEN + GOLD)");

    // Vérifier qu'on a bien les 3 assets
    let asset_ids: Vec<Option<String>> = alice_utxos
        .iter()
        .map(|(_, o)| o.asset_id.clone())
        .collect();
    assert!(asset_ids.contains(&None), "should contain PMS");
    assert!(
        asset_ids.contains(&Some("edenite".into())),
        "should contain edenite"
    );
    assert!(
        asset_ids.contains(&Some("gold".into())),
        "should contain gold"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// ShardedUtxoSet — apply_diff multi-asset
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn apply_diff_multi_asset() {
    let utxos = ShardedUtxoSet::new();

    // Seed initial
    seed_token(&utxos, "ff01", 0, "Alice", "100.00000000", "edenite").await;
    seed_pms(&utxos, "ff02", 0, "Alice", "5.00000000").await;

    // Simuler un transfer: Alice → Bob 60 EDEN, change 40 EDEN
    let spends = vec![out_id("ff01", 0), out_id("ff02", 0)];
    let creates = vec![
        (out_id("tx01", 0), token_output("Bob", "60.00000000", "edenite")),
        (out_id("tx01", 1), token_output("Alice", "40.00000000", "edenite")),
        (out_id("tx01", 2), pms_output("Alice", "4.90000000")),
        (out_id("tx01", 3), pms_output("FeePool", "0.10000000")),
    ];

    utxos.apply_diff(&spends, &creates).await;

    // Vérifier les balances après
    let alice_eden = utxos
        .balance_by_address_and_asset("Alice", Some("edenite"))
        .await;
    assert_eq!(alice_eden, Decimal::from_str("40.00000000").unwrap());

    let bob_eden = utxos
        .balance_by_address_and_asset("Bob", Some("edenite"))
        .await;
    assert_eq!(bob_eden, Decimal::from_str("60.00000000").unwrap());

    let alice_pms = utxos.balance_by_address("Alice").await;
    assert_eq!(alice_pms, Decimal::from_str("4.90000000").unwrap());

    // Supply totale EDEN inchangée
    let (eden_supply, _) = utxos.circulating_supply_by_asset(Some("edenite")).await;
    assert_eq!(eden_supply, Decimal::from_str("100.00000000").unwrap());

    // Old UTXOs should be gone
    assert!(utxos.get(&out_id("ff01", 0)).await.is_none());
    assert!(utxos.get(&out_id("ff02", 0)).await.is_none());
}

// ═══════════════════════════════════════════════════════════════════════════════
// TxOutput sérialisation — rétrocompatibilité
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn txoutput_without_asset_id_deserializes_as_none() {
    // Un ancien TxOutput sérialisé sans champ asset_id
    let json = r#"{"address":"Alice","amount":"10.0"}"#;
    let output: TxOutput = serde_json::from_str(json).unwrap();
    assert_eq!(output.asset_id, None, "missing asset_id should default to None");
}

#[test]
fn txoutput_with_asset_id_serializes_correctly() {
    let output = token_output("Bob", "50.0", "edenite");
    let json = serde_json::to_string(&output).unwrap();
    assert!(json.contains("\"asset_id\":\"edenite\""));
}

#[test]
fn txoutput_pms_skips_asset_id_in_json() {
    let output = pms_output("Alice", "10.0");
    let json = serde_json::to_string(&output).unwrap();
    assert!(
        !json.contains("asset_id"),
        "PMS output should not include asset_id in JSON"
    );
}
