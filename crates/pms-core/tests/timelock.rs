// crates/pms-core/tests/timelock.rs
//
// Tests du time-lock natif sur UTXO (protocole 2.1, v0.10.0).
//
// Un output portant `locked_until` (timestamp UNIX ms) est indépensable tant
// que l'horloge du validateur n'a pas atteint ce timestamp. Le lock vit dans
// l'output on-DAG, est propagé dans le cache RAM (`CompactOutput`) et dans
// RocksDB (`UtxoValue.lkd`), et est vérifié par `validate_transaction_full`
// (hot path) comme par le chemin legacy.
//
// Exécution :
//   cargo test --release -p pms-core --test timelock -- --nocapture

use pms_core::utxo::{ShardedUtxoSet, current_time_ms};
use pms_core::validations::check::ValidatePolicy;
use pms_core::validations::transactions::validate_transaction_full;
use pms_types::{OutputId, Transaction, TxInput, TxOutput, Unlock};
use pms_wallet::{SignerBackend, Wallet};

const NETWORK_ID: &str = "pms-testnet-v1";
const HOUR_MS: u64 = 3_600_000;

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

/// Signe `tx` avec `wallet`, un unlock par input (appariement positionnel).
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

/// Construit une dépense complète (owner → dest, montant intégral) de
/// l'UTXO `utxo_id`, signée par `owner`.
fn spend_all(owner: &Wallet, utxo_id: &OutputId, dest: &str, amount: &str) -> Transaction {
    let tx = Transaction {
        inputs: vec![TxInput {
            out: utxo_id.clone(),
        }],
        outputs: vec![TxOutput::new(dest, amount, None)],
        fee: "0".into(),
        unlocks: vec![],
    };
    sign_tx(owner, &tx, NETWORK_ID)
}

// ═══════════════════════════════════════════════════════════════════════════
// Cas malveillant : dépenser un UTXO encore verrouillé → rejet
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn locked_utxo_cannot_be_spent_before_expiry() {
    let owner = Wallet::from_seed(&[10u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");
    let dest = Wallet::from_seed(&[11u8; 32], None)
        .expect("wallet")
        .get_address("8e");

    let now = current_time_ms();
    let until = now + HOUR_MS; // verrouillé pour 1h

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(
            out_id("locked-mint", 0),
            TxOutput::new_locked(&owner_addr, "100.0", None, until),
        )
        .await;

    let tx = spend_all(&owner, &out_id("locked-mint", 0), &dest, "100.0");

    let result = validate_transaction_full(&utxos, &tx, &test_policy(), now, &Default::default()).await;
    println!("SPEND BEFORE EXPIRY (until={until}, now={now}) → {result:?}");
    let err = format!("{:?}", result.expect_err("locked spend MUST be rejected"));
    assert!(
        err.contains("OutputTimeLocked"),
        "expected OutputTimeLocked, got: {err}"
    );
    // L'erreur expose le timestamp du lock (public on-DAG) pour le client.
    assert!(err.contains(&until.to_string()), "err must carry `until`: {err}");
}

#[tokio::test]
async fn locked_utxo_rejected_even_one_ms_before_expiry() {
    let owner = Wallet::from_seed(&[12u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");

    let now = current_time_ms();
    let until = now + 1; // expire dans 1 ms exactement

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(
            out_id("edge-mint", 0),
            TxOutput::new_locked(&owner_addr, "5.0", None, until),
        )
        .await;

    let tx = spend_all(&owner, &out_id("edge-mint", 0), &owner_addr, "5.0");

    // Horloge figée à `now` : now < until → rejet (boundary case).
    let result = validate_transaction_full(&utxos, &tx, &test_policy(), now, &Default::default()).await;
    println!("BOUNDARY now={now} until={until} → {result:?}");
    assert!(
        format!("{:?}", result.expect_err("must reject")).contains("OutputTimeLocked")
    );

    // Horloge à `until` pile : now >= until → accepté (lock inclusif borné).
    let result_at = validate_transaction_full(&utxos, &tx, &test_policy(), until, &Default::default()).await;
    println!("AT-EXPIRY now={until} until={until} → {result_at:?}");
    assert!(result_at.is_ok(), "spend at exact expiry must pass: {result_at:?}");
}

// ═══════════════════════════════════════════════════════════════════════════
// Cas légitime : après expiration, la dépense passe
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn locked_utxo_spendable_after_expiry() {
    let owner = Wallet::from_seed(&[13u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");
    let dest = Wallet::from_seed(&[14u8; 32], None)
        .expect("wallet")
        .get_address("8e");

    let now = current_time_ms();
    let until = now.saturating_sub(HOUR_MS); // expiré depuis 1h

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(
            out_id("expired-mint", 0),
            TxOutput::new_locked(&owner_addr, "42.0", None, until),
        )
        .await;

    let tx = spend_all(&owner, &out_id("expired-mint", 0), &dest, "42.0");

    let result = validate_transaction_full(&utxos, &tx, &test_policy(), now, &Default::default()).await;
    println!("SPEND AFTER EXPIRY (until={until}, now={now}) → {result:?}");
    assert!(result.is_ok(), "expired lock must be spendable: {result:?}");
    let inputs = result.unwrap();
    println!("input outputs resolved: {inputs:?}");
    assert_eq!(inputs[0].locked_until, Some(until));
}

#[tokio::test]
async fn unlocked_utxo_unaffected_by_timelock_rule() {
    let owner = Wallet::from_seed(&[15u8; 32], None).expect("wallet");
    let owner_addr = owner.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(
            out_id("plain-mint", 0),
            TxOutput::new(&owner_addr, "7.0", None),
        )
        .await;

    let tx = spend_all(&owner, &out_id("plain-mint", 0), &owner_addr, "7.0");
    let result = validate_transaction_full(&utxos, &tx, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("PLAIN OUTPUT spend → {result:?}");
    assert!(result.is_ok(), "no-lock output must keep legacy behavior");
}

// ═══════════════════════════════════════════════════════════════════════════
// Piège cache : locked_until doit survivre au round-trip CompactOutput
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn locked_until_survives_utxo_cache_roundtrip() {
    let utxos = ShardedUtxoSet::new(0, None);
    let until = current_time_ms() + HOUR_MS;

    utxos
        .add(
            out_id("cache-rt", 3),
            TxOutput::new_locked("8e1someaddr", "9.99", Some("edenite".into()), until),
        )
        .await;

    let fetched = utxos.get(&out_id("cache-rt", 3)).await.expect("utxo");
    println!("cache round-trip: {fetched:?}");
    assert_eq!(fetched.locked_until, Some(until), "lock lost in CompactOutput!");
    assert_eq!(fetched.asset_id.as_deref(), Some("edenite"));
    assert_eq!(fetched.amount, "9.99");
}

// ═══════════════════════════════════════════════════════════════════════════
// Sélection de coins : les UTXOs encore verrouillés sont exclus
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn coin_selection_skips_still_locked_utxos() {
    use rust_decimal::Decimal;
    let utxos = ShardedUtxoSet::new(0, None);
    let addr = "8e1selectaddr";
    let now = current_time_ms();

    // 1 UTXO verrouillé (futur) + 1 UTXO libre + 1 UTXO au lock expiré
    utxos
        .add(
            out_id("sel-locked", 0),
            TxOutput::new_locked(addr, "100.0", None, now + HOUR_MS),
        )
        .await;
    utxos
        .add(out_id("sel-free", 0), TxOutput::new(addr, "10.0", None))
        .await;
    utxos
        .add(
            out_id("sel-expired", 0),
            TxOutput::new_locked(addr, "20.0", None, now.saturating_sub(1000)),
        )
        .await;

    let (selected, total) = utxos
        .utxos_by_address_for_selection(addr, &None, Decimal::from(1000), 256)
        .await;

    let ids: Vec<&str> = selected.iter().map(|(id, _, _)| id.txid.as_str()).collect();
    println!("selected: {ids:?}, total={total}");
    assert!(!ids.contains(&"sel-locked"), "locked UTXO must be skipped");
    assert!(ids.contains(&"sel-free"));
    assert!(ids.contains(&"sel-expired"), "expired lock is spendable");
    assert_eq!(total, Decimal::from(30), "total = 10 + 20 (lock exclu)");
}
