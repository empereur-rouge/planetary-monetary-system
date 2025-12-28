use anyhow::Result;
use pms_core::{validate_block, Dag, ValidatePolicy};
use pms_types::{Block, BlockId, OutputId, PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput, Unlock};
use pms_utils::compute_block_id;

/// Helper: nouveau bloc (parents + payload) avec id/nonce calculés.
fn mined(parents: Vec<BlockId>, payload: Option<PayloadEnvelope>) -> Block {
    // en tests on ne fait pas de PoW -> nonce=1, compute_id = stable
    let nonce = 1;
    let id = compute_block_id(&parents, &payload, nonce);
    Block { id, parents, payload, nonce }
}

/// Construis une Mint simple vers `addr`, renvoie le bloc + (txid, index) dépensable.
fn mint_block(addr: &str, amount: &str, parent: &str) -> (Block, OutputId) {
    let txo = TxOutput { address: addr.into(), amount: amount.into() };
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Mint { outputs: vec![txo] }));
    let b = mined(vec![parent.into()], payload);
    let out_id = OutputId { txid: b.id.clone(), index: 0 };
    (b, out_id)
}

/// Validation d’un tx légitime qui dépense une mint unique.
#[test]
fn accept_valid_tx() -> Result<()> {
    // DAG + genesis
    let mut dag = Dag::new_with_genesis(Block::genesis(compute_block_id));
    let policy = ValidatePolicy::default();

    // 1) Mint vers A
    let (b1, spendable) = mint_block("A", "10.0", &dag.blocks.keys().next().unwrap());
    validate_block(&dag, &b1, &policy).expect("mint valide");
    dag.add_block(b1.clone()).unwrap();

    // 2) Tx qui dépense la mint -> B
    let tx = Transaction {
        inputs: vec![TxInput { out: spendable.clone() }],
        outputs: vec![TxOutput { address: "B".into(), amount: "9.0".into() }],
        fee: "1.0".into(),
        unlocks: vec![Unlock { pubkey_hex: "A_pub".into(), signature_b64: "sig".into() }],
    };
    let b2 = mined(vec![b1.id.clone()], Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx))));
    validate_block(&dag, &b2, &policy).expect("tx valide doit être accepté");
    Ok(())
}

/// Double-spend à travers 2 blocs différents : le 2e doit être rejeté.
#[test]
fn reject_double_spend_intra_block() -> Result<()> {
    let mut dag = Dag::new_with_genesis(Block::genesis(compute_block_id));
    let policy = ValidatePolicy::default();
    let genesis_id = dag.blocks.keys().next().unwrap().clone();

    // Mint -> A
    let (b1, spendable) = mint_block("A", "5.0", &genesis_id);
    validate_block(&dag, &b1, &policy).unwrap();
    dag.add_block(b1.clone()).unwrap();

    // Tx1 dépense l’output
    let tx1 = Transaction {
        inputs: vec![TxInput { out: spendable.clone() }],
        outputs: vec![TxOutput { address: "A".into(), amount: "4.5".into() }],
        fee: "0.5".into(),
        unlocks: vec![Unlock { pubkey_hex: "A_pub".into(), signature_b64: "sig1".into() }],
    };
    let b2 = mined(vec![b1.id.clone()], Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx1))));
    validate_block(&dag, &b2, &policy).unwrap();
    dag.add_block(b2.clone()).unwrap();

    // Tx2 re-dépense le même input -> doit être rejeté
    let tx2 = Transaction {
        inputs: vec![TxInput { out: spendable.clone() }],
        outputs: vec![TxOutput { address: "A".into(), amount: "4.0".into() }],
        fee: "1.0".into(),
        unlocks: vec![Unlock { pubkey_hex: "A_pub".into(), signature_b64: "sig2".into() }],
    };
    let b3 = mined(vec![b2.id.clone()], Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx2))));
    let err = validate_block(&dag, &b3, &policy).expect_err("double-spend non détecté");
    assert!(format!("{err:?}").contains("DoubleSpend"));
    Ok(())
}
