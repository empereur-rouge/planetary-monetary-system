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
    let txo = TxOutput {
        address: addr.into(),
        amount: amount.into(),
    };
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

/// Create a properly signed transaction
fn sign_transaction(wallet: &Wallet, tx: &Transaction) -> Transaction {
    // Transaction::signing_message() returns the canonical message hex
    let msg_hex = tx.signing_message().expect("signing_message");
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
        outputs: vec![TxOutput {
            address: "B".into(),
            amount: "9.0".into(),
        }],
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
        outputs: vec![TxOutput {
            address: wallet_addr.clone(),
            amount: "4.5".into(),
        }],
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
        outputs: vec![TxOutput {
            address: wallet_addr.clone(),
            amount: "4.0".into(),
        }],
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
