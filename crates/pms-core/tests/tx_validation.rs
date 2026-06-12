use anyhow::Result;
use pms_core::{Dag, ValidatePolicy, validate_block};
use pms_types::{
    Block, BlockId, OutputId, PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput, Unlock,
};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};

/// Helper: nouveau bloc (parents + payload) avec id/nonce calculés.
fn mined(parents: Vec<BlockId>, payload: Option<PayloadEnvelope>) -> Block {
    let nonce = 1;
    let id = compute_block_id(&parents, &payload, nonce);
    Block {
        id,
        parents,
        payload,
        nonce,
        metadata: None,
        signer_pk: None,
        signature: None,
    }
}

/// Construis une Mint simple vers `addr`, renvoie le bloc + (txid, index) dépensable.
fn mint_block(addr: &str, amount: &str, parent: &str) -> (Block, OutputId) {
    let txo = TxOutput::new(addr, amount, None);
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Mint {
        outputs: vec![txo],
    }));
    let b = mined(vec![parent.into()], payload);
    let out_id = OutputId {
        txid: b.id.clone(),
        index: 0,
    };
    (b, out_id)
}

/// Sign a TX for a given network. `""` matches `ValidatePolicy::default()`
/// (no replay binding), used by the legacy tests in this file.
fn sign_transaction_for(wallet: &Wallet, tx: &Transaction, network_id: &str) -> Transaction {
    let msg_hex = tx.signing_message(network_id).expect("signing_message");
    let sig = wallet.sign(&msg_hex).expect("sign");
    Transaction {
        inputs: tx.inputs.clone(),
        outputs: tx.outputs.clone(),
        fee: tx.fee.clone(),
        unlocks: vec![Unlock {
            pubkey_hex: wallet.encoded_public_key(),
            signature_b64: sig,
        }],
    }
}

fn sign_transaction(wallet: &Wallet, tx: &Transaction) -> Transaction {
    sign_transaction_for(wallet, tx, "")
}

/// Validation d'un tx légitime qui dépense une mint unique.
#[test]
fn accept_valid_tx() -> Result<()> {
    // DAG + genesis
    let mut dag = Dag::new_with_genesis(Block::genesis(compute_block_id));
    let policy = ValidatePolicy::default();

    // Wallet de test
    let wallet = Wallet::from_seed(&[42u8; 32], None).expect("wallet");
    let wallet_addr = wallet.get_address("8e");

    // 1) Mint vers wallet address
    let genesis_id = dag.blocks.keys().next().unwrap().clone();
    let (b1, spendable) = mint_block(&wallet_addr, "10.0", &genesis_id);
    validate_block(&dag, &b1, &policy).expect("mint valide");
    dag.add_block(b1.clone()).unwrap();

    // 2) Tx qui dépense la mint -> B (unsigned first)
    let tx_unsigned = Transaction {
        inputs: vec![TxInput {
            out: spendable.clone(),
        }],
        outputs: vec![TxOutput::new("B", "9.0", None)],
        fee: "1.0".into(),
        unlocks: vec![],
    };

    let tx_signed = sign_transaction(&wallet, &tx_unsigned);

    let b2 = mined(
        vec![b1.id.clone()],
        Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx_signed))),
    );
    validate_block(&dag, &b2, &policy).expect("tx valide doit être accepté");
    Ok(())
}

/// Double-spend à travers 2 blocs différents : le 2e doit être rejeté.
#[test]
fn reject_double_spend_intra_block() -> Result<()> {
    let mut dag = Dag::new_with_genesis(Block::genesis(compute_block_id));
    let policy = ValidatePolicy::default();
    let genesis_id = dag.blocks.keys().next().unwrap().clone();

    // Wallet de test
    let wallet = Wallet::from_seed(&[42u8; 32], None).expect("wallet");
    let wallet_addr = wallet.get_address("8e");

    // Mint -> wallet address
    let (b1, spendable) = mint_block(&wallet_addr, "5.0", &genesis_id);
    validate_block(&dag, &b1, &policy).unwrap();
    dag.add_block(b1.clone()).unwrap();

    // Tx1 dépense l'output
    let tx1_unsigned = Transaction {
        inputs: vec![TxInput {
            out: spendable.clone(),
        }],
        outputs: vec![TxOutput::new(wallet_addr.clone(), "4.5", None)],
        fee: "0.5".into(),
        unlocks: vec![],
    };
    let tx1_signed = sign_transaction(&wallet, &tx1_unsigned);

    let b2 = mined(
        vec![b1.id.clone()],
        Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx1_signed))),
    );
    validate_block(&dag, &b2, &policy).unwrap();
    dag.add_block(b2.clone()).unwrap();

    // Tx2 re-dépense le même input -> doit être rejeté
    let tx2_unsigned = Transaction {
        inputs: vec![TxInput {
            out: spendable.clone(),
        }],
        outputs: vec![TxOutput::new(wallet_addr.clone(), "4.0", None)],
        fee: "1.0".into(),
        unlocks: vec![],
    };
    let tx2_signed = sign_transaction(&wallet, &tx2_unsigned);

    let b3 = mined(
        vec![b2.id.clone()],
        Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx2_signed))),
    );
    let err = validate_block(&dag, &b3, &policy).expect_err("double-spend non détecté");
    assert!(format!("{err:?}").contains("DoubleSpend") || format!("{err:?}").contains("spent"));
    Ok(())
}

/// Cross-chain replay protection: a TX signed for one network_id MUST be
/// rejected by a verifier configured for a different network_id. Without
/// this protection, a TX broadcast on testnet could be replayed verbatim on
/// mainnet (same UTXO model, same coordinator key) and drain real funds.
#[test]
fn reject_tx_signed_for_different_network() -> Result<()> {
    let mut dag = Dag::new_with_genesis(Block::genesis(compute_block_id));

    // The "mainnet" verifier — runs with network_id = "pms-mainnet-v1".
    let mut policy_mainnet = ValidatePolicy::default();
    policy_mainnet.network_id = "pms-mainnet-v1".to_string();

    let wallet = Wallet::from_seed(&[7u8; 32], None).expect("wallet");
    let wallet_addr = wallet.get_address("8e");

    // Mint UTXOs to the wallet. Mint payloads carry no signature so the
    // network_id constraint doesn't apply here — the mint is fine on any
    // network configuration.
    let genesis_id = dag.blocks.keys().next().unwrap().clone();
    let (b_mint, spendable) = mint_block(&wallet_addr, "10.0", &genesis_id);
    validate_block(&dag, &b_mint, &policy_mainnet).expect("mint valide");
    dag.add_block(b_mint.clone()).unwrap();

    // Build a TX and sign it as if for testnet (the "stolen" TX an
    // attacker would replay against mainnet).
    let tx_unsigned = Transaction {
        inputs: vec![TxInput {
            out: spendable.clone(),
        }],
        outputs: vec![TxOutput::new("B", "9.0", None)],
        fee: "1.0".into(),
        unlocks: vec![],
    };

    // Sign for testnet — wrong network for the mainnet verifier below.
    let tx_replayed = sign_transaction_for(&wallet, &tx_unsigned, "pms-testnet-v1");

    let b_replay = mined(
        vec![b_mint.id.clone()],
        Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx_replayed))),
    );

    // The mainnet verifier MUST reject — signature doesn't cover its network_id.
    let err = validate_block(&dag, &b_replay, &policy_mainnet)
        .expect_err("cross-chain replay must be rejected");
    let err_str = format!("{err:?}");
    println!("CROSS-CHAIN REPLAY rejected with: {}", err_str);
    assert!(
        err_str.contains("InvalidSignature") || err_str.contains("signature"),
        "expected signature-related error, got: {}",
        err_str
    );

    // Sanity: the SAME TX, re-signed for mainnet, validates on mainnet.
    let tx_correct = sign_transaction_for(&wallet, &tx_unsigned, "pms-mainnet-v1");
    let b_correct = mined(
        vec![b_mint.id.clone()],
        Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx_correct))),
    );
    validate_block(&dag, &b_correct, &policy_mainnet)
        .expect("TX signed for mainnet must validate on mainnet");
    println!("CONTROL: same TX re-signed for pms-mainnet-v1 accepted");

    Ok(())
}

/// Same-network sanity: a TX signed for "X" must validate on a "X" verifier
/// (proves the new network_id binding doesn't accidentally break the legit path).
#[test]
fn accept_tx_signed_for_matching_network() -> Result<()> {
    let mut dag = Dag::new_with_genesis(Block::genesis(compute_block_id));
    let mut policy = ValidatePolicy::default();
    policy.network_id = "pms-testnet-v1".to_string();

    let wallet = Wallet::from_seed(&[9u8; 32], None).expect("wallet");
    let wallet_addr = wallet.get_address("8e");

    let genesis_id = dag.blocks.keys().next().unwrap().clone();
    let (b1, spendable) = mint_block(&wallet_addr, "5.0", &genesis_id);
    validate_block(&dag, &b1, &policy).expect("mint valide");
    dag.add_block(b1.clone()).unwrap();

    let tx_unsigned = Transaction {
        inputs: vec![TxInput { out: spendable }],
        outputs: vec![TxOutput::new("C", "4.5", None)],
        fee: "0.5".into(),
        unlocks: vec![],
    };
    let tx_signed = sign_transaction_for(&wallet, &tx_unsigned, &policy.network_id);

    let b2 = mined(
        vec![b1.id.clone()],
        Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx_signed))),
    );
    validate_block(&dag, &b2, &policy).expect("matching-network tx must validate");
    println!("CONTROL: TX signed for {} accepted on matching verifier", policy.network_id);
    Ok(())
}
