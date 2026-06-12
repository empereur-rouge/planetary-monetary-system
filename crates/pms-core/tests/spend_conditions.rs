// crates/pms-core/tests/spend_conditions.rs
//
// Tests des conditions de déverrouillage (protocole 2.2, v0.10.0) :
// MultiSig M-of-N et HashLock, intégrées au pipeline C-1
// (`validate_transaction_full` → `check_spend_authorization`).
//
// Exécution :
//   cargo test --release -p pms-core --test spend_conditions -- --nocapture

use pms_core::utxo::{ShardedUtxoSet, current_time_ms};
use pms_core::validations::check::ValidatePolicy;
use pms_core::validations::conditions::multisig_address;
use pms_core::validations::transactions::validate_transaction_full;
use pms_types::{Cosigner, OutputId, SpendCondition, Transaction, TxInput, TxOutput, Unlock};
use pms_wallet::{SignerBackend, Wallet};
use sha2::{Digest, Sha256};

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

fn wallet(seed: u8) -> Wallet {
    Wallet::from_seed(&[seed; 32], None).expect("wallet")
}

/// Un output sous condition MultiSig M-of-N, à l'adresse canonique de la policy.
fn multisig_output(amount: &str, m: u8, wallets: &[&Wallet]) -> TxOutput {
    let pubkeys: Vec<String> = wallets.iter().map(|w| w.public_key_hex.clone()).collect();
    TxOutput {
        address: multisig_address(m, &pubkeys),
        amount: amount.into(),
        asset_id: None,
        locked_until: None,
        spend_condition: Some(SpendCondition::MultiSig { m, pubkeys }),
        created_at: None,
    }
}

/// Un output sous condition HashLock(SHA256(preimage)).
fn hashlock_output(address: &str, amount: &str, preimage: &[u8]) -> TxOutput {
    TxOutput {
        address: address.into(),
        amount: amount.into(),
        asset_id: None,
        locked_until: None,
        spend_condition: Some(SpendCondition::HashLock {
            hash_hex: hex::encode(Sha256::digest(preimage)),
        }),
        created_at: None,
    }
}

/// Transaction non signée : dépense intégrale de `utxo_id` vers `dest`.
fn unsigned_spend(utxo_id: &OutputId, dest: &str, amount: &str) -> Transaction {
    Transaction {
        inputs: vec![TxInput {
            out: utxo_id.clone(),
        }],
        outputs: vec![TxOutput::new(dest, amount, None)],
        fee: "0".into(),
        unlocks: vec![],
    }
}

/// Signe le message canonique de `tx` avec chaque wallet de `signers` et
/// construit UN unlock (premier signataire en principal, les autres en
/// cosigners), optionnellement avec un préimage.
fn sign_with(
    tx: &Transaction,
    signers: &[&Wallet],
    preimage_hex: Option<String>,
) -> Transaction {
    let msg = tx.signing_message(NETWORK_ID).expect("signing_message");
    let mut sigs: Vec<(String, String)> = signers
        .iter()
        .map(|w| (w.public_key_hex.clone(), w.sign(&msg).expect("sign")))
        .collect();
    let (primary_pk, primary_sig) = sigs.remove(0);
    let unlock = Unlock {
        pubkey_hex: primary_pk,
        signature_b64: primary_sig,
        cosigners: sigs
            .into_iter()
            .map(|(pk, sig)| Cosigner {
                pubkey_hex: pk,
                signature_b64: sig,
            })
            .collect(),
        preimage_hex,
    };
    Transaction {
        inputs: tx.inputs.clone(),
        outputs: tx.outputs.clone(),
        fee: tx.fee.clone(),
        unlocks: vec![unlock],
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// MultiSig M-of-N
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn multisig_2of3_with_two_signers_accepted() {
    let (a, b, c) = (wallet(20), wallet(21), wallet(22));
    let dest = wallet(23).get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("msig-fund", 0), multisig_output("100.0", 2, &[&a, &b, &c]))
        .await;

    let tx = unsigned_spend(&out_id("msig-fund", 0), &dest, "100.0");
    let signed = sign_with(&tx, &[&a, &c], None); // 2 des 3 clés

    let result = validate_transaction_full(&utxos, &signed, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("MULTISIG 2-of-3 (signers a+c) → {result:?}");
    assert!(result.is_ok(), "quorum 2/3 atteint doit passer: {result:?}");
}

#[tokio::test]
async fn multisig_2of3_with_single_signer_rejected() {
    let (a, b, c) = (wallet(24), wallet(25), wallet(26));
    let dest = wallet(27).get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("msig-fund2", 0), multisig_output("50.0", 2, &[&a, &b, &c]))
        .await;

    let tx = unsigned_spend(&out_id("msig-fund2", 0), &dest, "50.0");
    let signed = sign_with(&tx, &[&b], None); // 1 seule signature

    let result = validate_transaction_full(&utxos, &signed, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("MULTISIG 2-of-3 (1 signer) → {result:?}");
    let err = format!("{:?}", result.expect_err("quorum non atteint DOIT être rejeté"));
    assert!(err.contains("SpendConditionNotMet"), "got: {err}");
}

#[tokio::test]
async fn multisig_duplicate_cosigner_does_not_fake_quorum() {
    let (a, b) = (wallet(28), wallet(29));
    let dest = wallet(30).get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("msig-dup", 0), multisig_output("10.0", 2, &[&a, &b]))
        .await;

    // a signe deux fois (principal + cosigner identique) → 1 seule clé distincte
    let tx = unsigned_spend(&out_id("msig-dup", 0), &dest, "10.0");
    let signed = sign_with(&tx, &[&a, &a], None);

    let result = validate_transaction_full(&utxos, &signed, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("MULTISIG quorum-stuffing (a+a pour m=2) → {result:?}");
    let err = format!("{:?}", result.expect_err("doublon ne doit pas compter 2x"));
    assert!(err.contains("SpendConditionNotMet"), "got: {err}");
}

#[tokio::test]
async fn multisig_outsider_signature_does_not_count() {
    let (a, b, outsider) = (wallet(31), wallet(32), wallet(33));
    let dest = wallet(34).get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("msig-out", 0), multisig_output("10.0", 2, &[&a, &b]))
        .await;

    // a (membre) + outsider (hors set) → 1 seule clé du set
    let tx = unsigned_spend(&out_id("msig-out", 0), &dest, "10.0");
    let signed = sign_with(&tx, &[&a, &outsider], None);

    let result = validate_transaction_full(&utxos, &signed, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("MULTISIG outsider cosigner → {result:?}");
    let err = format!("{:?}", result.expect_err("clé hors set ne compte pas"));
    assert!(err.contains("SpendConditionNotMet"), "got: {err}");
}

#[tokio::test]
async fn multisig_invalid_cosignature_rejected_by_crypto_check() {
    let (a, b) = (wallet(35), wallet(36));
    let dest = wallet(37).get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("msig-badsig", 0), multisig_output("10.0", 2, &[&a, &b]))
        .await;

    // b « cosigne » avec une signature forgée (signature de a recopiée).
    let tx = unsigned_spend(&out_id("msig-badsig", 0), &dest, "10.0");
    let msg = tx.signing_message(NETWORK_ID).unwrap();
    let sig_a = a.sign(&msg).unwrap();
    let forged = Transaction {
        inputs: tx.inputs.clone(),
        outputs: tx.outputs.clone(),
        fee: tx.fee.clone(),
        unlocks: vec![Unlock {
            pubkey_hex: a.public_key_hex.clone(),
            signature_b64: sig_a.clone(),
            cosigners: vec![Cosigner {
                pubkey_hex: b.public_key_hex.clone(),
                signature_b64: sig_a, // signature de a sous la clé de b
            }],
            preimage_hex: None,
        }],
    };

    let result = validate_transaction_full(&utxos, &forged, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("MULTISIG forged cosignature → {result:?}");
    let err = format!("{:?}", result.expect_err("cosignature forgée DOIT être rejetée"));
    assert!(err.contains("InvalidSignature"), "got: {err}");
}

#[tokio::test]
async fn multisig_output_with_wrong_address_rejected_at_creation() {
    let (a, b, owner) = (wallet(38), wallet(39), wallet(40));
    let owner_addr = owner.get_address("8e");

    // owner possède un UTXO simple et tente de créer un output MultiSig dont
    // l'adresse N'EST PAS l'adresse canonique de la policy.
    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("fund-owner", 0), TxOutput::new(&owner_addr, "10.0", None))
        .await;

    let pubkeys = vec![a.public_key_hex.clone(), b.public_key_hex.clone()];
    let bogus = TxOutput {
        address: "msig1deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into(),
        amount: "10.0".into(),
        asset_id: None,
        locked_until: None,
        spend_condition: Some(SpendCondition::MultiSig { m: 2, pubkeys }),
        created_at: None,
    };
    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("fund-owner", 0),
        }],
        outputs: vec![bogus],
        fee: "0".into(),
        unlocks: vec![],
    };
    let signed = sign_with(&tx, &[&owner], None);

    let result = validate_transaction_full(&utxos, &signed, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("MULTISIG wrong-address output → {result:?}");
    let err = format!("{:?}", result.expect_err("adresse non canonique rejetée"));
    assert!(err.contains("InvalidSpendCondition"), "got: {err}");
}

#[tokio::test]
async fn multisig_full_lifecycle_fund_then_spend() {
    // Cycle complet : owner fonde le multisig (output bien formé), puis le
    // quorum dépense vers une adresse simple.
    let (a, b, owner) = (wallet(41), wallet(42), wallet(43));
    let owner_addr = owner.get_address("8e");
    let dest = wallet(44).get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("fund-src", 0), TxOutput::new(&owner_addr, "25.0", None))
        .await;

    // 1) owner → multisig 2-of-2 (adresse canonique)
    let msig_out = multisig_output("25.0", 2, &[&a, &b]);
    let msig_addr = msig_out.address.clone();
    let fund = Transaction {
        inputs: vec![TxInput {
            out: out_id("fund-src", 0),
        }],
        outputs: vec![msig_out.clone()],
        fee: "0".into(),
        unlocks: vec![],
    };
    let fund_signed = sign_with(&fund, &[&owner], None);
    let r1 = validate_transaction_full(&utxos, &fund_signed, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("FUND multisig (addr={msig_addr}) → {r1:?}");
    assert!(r1.is_ok(), "funding tx must pass: {r1:?}");

    // Applique le funding (simule le persist)
    utxos.apply_diff(&[out_id("fund-src", 0)], &[(out_id("fund-tx", 0), msig_out)]).await;

    // 2) a+b dépensent le multisig
    let spend = unsigned_spend(&out_id("fund-tx", 0), &dest, "25.0");
    let spend_signed = sign_with(&spend, &[&a, &b], None);
    let r2 = validate_transaction_full(&utxos, &spend_signed, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("SPEND multisig 2-of-2 → {r2:?}");
    assert!(r2.is_ok(), "quorum spend must pass: {r2:?}");
}

// ═══════════════════════════════════════════════════════════════════════════
// HashLock
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn hashlock_correct_preimage_accepted() {
    let spender = wallet(50);
    let dest = spender.get_address("8e");
    let preimage = b"secret-pms-hashlock-001";

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(
            out_id("hl-fund", 0),
            hashlock_output("8e1hashlockdest", "15.0", preimage),
        )
        .await;

    let tx = unsigned_spend(&out_id("hl-fund", 0), &dest, "15.0");
    let signed = sign_with(&tx, &[&spender], Some(hex::encode(preimage)));

    let result = validate_transaction_full(&utxos, &signed, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("HASHLOCK correct preimage → {result:?}");
    assert!(result.is_ok(), "preimage correct doit passer: {result:?}");
}

#[tokio::test]
async fn hashlock_wrong_preimage_rejected() {
    let spender = wallet(51);
    let dest = spender.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(
            out_id("hl-fund2", 0),
            hashlock_output("8e1hashlockdest", "15.0", b"the-real-secret"),
        )
        .await;

    let tx = unsigned_spend(&out_id("hl-fund2", 0), &dest, "15.0");
    let signed = sign_with(&tx, &[&spender], Some(hex::encode(b"wrong-guess")));

    let result = validate_transaction_full(&utxos, &signed, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("HASHLOCK wrong preimage → {result:?}");
    let err = format!("{:?}", result.expect_err("mauvais preimage rejeté"));
    assert!(err.contains("SpendConditionNotMet"), "got: {err}");
}

#[tokio::test]
async fn hashlock_missing_preimage_rejected() {
    let spender = wallet(52);
    let dest = spender.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(
            out_id("hl-fund3", 0),
            hashlock_output("8e1hashlockdest", "15.0", b"another-secret"),
        )
        .await;

    let tx = unsigned_spend(&out_id("hl-fund3", 0), &dest, "15.0");
    let signed = sign_with(&tx, &[&spender], None); // pas de preimage

    let result = validate_transaction_full(&utxos, &signed, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("HASHLOCK missing preimage → {result:?}");
    let err = format!("{:?}", result.expect_err("preimage absent rejeté"));
    assert!(err.contains("SpendConditionNotMet"), "got: {err}");
}

#[tokio::test]
async fn hashlock_output_with_malformed_hash_rejected_at_creation() {
    let owner = wallet(53);
    let owner_addr = owner.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    utxos
        .add(out_id("hl-src", 0), TxOutput::new(&owner_addr, "5.0", None))
        .await;

    let bad = TxOutput {
        address: "8e1whatever".into(),
        amount: "5.0".into(),
        asset_id: None,
        locked_until: None,
        spend_condition: Some(SpendCondition::HashLock {
            hash_hex: "not-64-hex".into(),
        }),
        created_at: None,
    };
    let tx = Transaction {
        inputs: vec![TxInput {
            out: out_id("hl-src", 0),
        }],
        outputs: vec![bad],
        fee: "0".into(),
        unlocks: vec![],
    };
    let signed = sign_with(&tx, &[&owner], None);

    let result = validate_transaction_full(&utxos, &signed, &test_policy(), current_time_ms(), &Default::default()).await;
    println!("HASHLOCK malformed hash at creation → {result:?}");
    let err = format!("{:?}", result.expect_err("hash mal formé rejeté"));
    assert!(err.contains("InvalidSpendCondition"), "got: {err}");
}

// ═══════════════════════════════════════════════════════════════════════════
// PubKey explicite + round-trip cache
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn explicit_pubkey_condition_behaves_like_none() {
    let owner = wallet(54);
    let attacker = wallet(55);
    let owner_addr = owner.get_address("8e");

    let utxos = ShardedUtxoSet::new(0, None);
    let mut out = TxOutput::new(&owner_addr, "8.0", None);
    out.spend_condition = Some(SpendCondition::PubKey);
    utxos.add(out_id("pk-fund", 0), out).await;

    // Légitime
    let tx = unsigned_spend(&out_id("pk-fund", 0), &owner_addr, "8.0");
    let ok = validate_transaction_full(
        &utxos,
        &sign_with(&tx, &[&owner], None),
        &test_policy(),
        current_time_ms(),
        &Default::default(),
    )
    .await;
    println!("PUBKEY explicit, owner spend → {ok:?}");
    assert!(ok.is_ok());

    // Vol : un tiers signe avec sa propre clé → binding C-1
    let theft = validate_transaction_full(
        &utxos,
        &sign_with(&tx, &[&attacker], None),
        &test_policy(),
        current_time_ms(),
        &Default::default(),
    )
    .await;
    println!("PUBKEY explicit, attacker spend → {theft:?}");
    let err = format!("{:?}", theft.expect_err("C-1 binding doit tenir"));
    assert!(err.contains("OwnershipMismatch"), "got: {err}");
}

#[tokio::test]
async fn spend_condition_survives_utxo_cache_roundtrip() {
    let (a, b) = (wallet(56), wallet(57));
    let utxos = ShardedUtxoSet::new(0, None);
    let out = multisig_output("3.0", 2, &[&a, &b]);
    let expected_cond = out.spend_condition.clone();

    utxos.add(out_id("cond-rt", 1), out).await;
    let fetched = utxos.get(&out_id("cond-rt", 1)).await.expect("utxo");
    println!("cache round-trip condition: {:?}", fetched.spend_condition);
    assert_eq!(fetched.spend_condition, expected_cond, "condition lost in cache!");
}
