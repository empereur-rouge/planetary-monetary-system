// crates/pms-wallet/tests/rocks_decrypt_and_filter.rs

use anyhow::Result;
use pms_storage::{DagStorage, models::StoredBlock};
use pms_testkit::test_rocks_store;
use pms_types_block::Block;
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_types_transaction::{OutputId, Transaction, TxInput, TxOutput, Unlock};
use pms_utils::compute_block_id;
use pms_wallet::Wallet;
use pms_wallet::history::scan_decrypt_recent_for_address;
use pms_wire::WireMeta;

// petit helper: construit un StoredBlock avec id calculé depuis (parents, payload, nonce)
fn mk_sb(
    id: String,
    parents: Vec<String>,
    payload: &PayloadEnvelope,
    nonce: u64,
    meta: &WireMeta,
) -> StoredBlock {
    StoredBlock {
        id,
        parents,
        payload_json: serde_json::to_string(payload).ok(),
        nonce,
        // Champs obligatoires ajoutés
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    }
}

#[tokio::test]
async fn decrypt_and_filter_by_address_rocks() -> Result<()> {
    // ----- Arrange (RocksDB éphémère) -----
    let tr = test_rocks_store("hist-decrypt-filter").await?;
    let store = tr.store.clone();

    let settings = pms_config::load_config()?;
    let meta = pms_wire::WireMeta::from(&settings);

    // Genesis en base (idempotent)
    let g = Block::genesis(compute_block_id);
    let gsb = StoredBlock {
        id: g.id.clone(),
        parents: vec![],
        payload_json: serde_json::to_string(&g.payload).ok(),
        nonce: g.nonce,

        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(), // genesis non signé
        signature_hex: String::new(),
        metadata: None,
    };
    let _ = store.append_block_atomic(&gsb).await?;

    // Wallet cible (doit avoir mnemonic pour extraire la sk x25519)
    let w = Wallet::generate();
    let my_addr = w.get_address(&settings.address.hrp);
    let my_xpk = w.x25519_pub_hex.clone();
    let my_sk = w.x25519_sk_hex().expect("wallet must hold mnemonics");

    // 1) Bloc Mint -> m'envoie 42
    let plain_mint = PlainPayload::Mint {
        outputs: vec![TxOutput {
            address: my_addr.clone(),
            amount: "42".into(),
        }],
    };
    let enc_mint = EncryptedPayload::encrypt_for_plain(&plain_mint, &[my_xpk.clone()])
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let wb_mint_parents = vec![g.id.clone()];
    let mint_id = compute_block_id(
        &wb_mint_parents,
        &serde_json::to_string(&PayloadEnvelope::Encrypted(enc_mint.clone()))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok()),
        1,
    );
    let sb_mint = mk_sb(
        mint_id,
        wb_mint_parents.clone(),
        &PayloadEnvelope::Encrypted(enc_mint),
        1,
        &meta,
    );
    let _ = store.append_block_atomic(&sb_mint).await?;

    // 2) Bloc Tx chiffré (pour moi aussi) mais qui ne m’envoie rien -> doit être filtré
    let other_addr = "8e1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
    let tx_plain = PlainPayload::TxUtxo(Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: "prev".into(),
                index: 0,
            },
        }],
        outputs: vec![TxOutput {
            address: other_addr.into(),
            amount: "13".into(),
        }],
        fee: "0".into(),
        unlocks: vec![Unlock {
            pubkey_hex: "00".into(),
            signature_b64: "AA==".into(),
        }],
    });
    let enc_tx = EncryptedPayload::encrypt_for_plain(&tx_plain, &[my_xpk.clone()])
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let wb_tx_parents = vec![g.id.clone()];
    let tx_id = compute_block_id(
        &wb_tx_parents,
        &serde_json::to_string(&PayloadEnvelope::Encrypted(enc_tx.clone()))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok()),
        2,
    );
    let sb_tx = mk_sb(
        tx_id,
        wb_tx_parents,
        &PayloadEnvelope::Encrypted(enc_tx),
        2,
        &meta,
    );
    let _ = store.append_block_atomic(&sb_tx).await?;

    // ----- Act -----
    let dec = scan_decrypt_recent_for_address(&*store, &my_sk, &my_addr, 200).await?;

    // ----- Assert -----
    // On ne garde que le Mint (la Tx ne m’implique pas en outputs)
    assert_eq!(
        dec.len(),
        1,
        "Un seul bloc doit concerner mon adresse (le Mint)"
    );
    match &dec[0].plain {
        PlainPayload::Mint { outputs } => {
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].address, my_addr);
            assert_eq!(outputs[0].amount, "42");
        }
        other => panic!("Attendu Mint, reçu: {:?}", other),
    }

    Ok(())
}
