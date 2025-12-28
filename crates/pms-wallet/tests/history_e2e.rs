// crates/pms-wallet/tests/rocks_history_e2e.rs

use anyhow::Result;
use tokio::time::{sleep, Duration};

use rand::{rngs::OsRng, RngCore};

use pms_config::load_config;
use pms_storage::{models::StoredBlock, DagStorage};
use pms_testkit::test_rocks_store;
use pms_utils::compute_block_id;

use pms_wallet::{Wallet, history::scan_decrypt_recent_for_address};

use pms_types_block::Block;
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_types_transaction::{OutputId, Transaction, TxInput, TxOutput, Unlock};
use pms_wire::WireMeta;

fn ns() -> String {
    let mut r = [0u8; 4];
    OsRng.fill_bytes(&mut r);
    format!("it:history:e2e:{:02x}{:02x}{:02x}{:02x}", r[0], r[1], r[2], r[3])
}

#[tokio::test]
async fn history_e2e_scan_decrypt_filter_by_address_rocks() -> Result<()> {
    // ----- Arrange (Rocks) -----
    let tr = test_rocks_store("history-e2e").await?;
    let store = tr.store.clone();

    // Config (pour HRP bech32)
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // Genesis
    let g = Block::genesis(compute_block_id);
    let g_sb = StoredBlock {
        id: g.id.clone(),
        parents: vec![],
        payload_json: serde_json::to_string(&g.payload).ok(),
        nonce: g.nonce,

        network_id:       meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex:    String::new(),
        signature_hex:    String::new(),
    };
    let _ = store.append_block_atomic(&g_sb).await?;

    // Wallet cible
    let w = Wallet::generate();
    let my_addr = w.get_address(&settings.address.hrp);
    let my_xpk  = w.x25519_pub_hex.clone();
    let my_sk   = w.x25519_sk_hex().expect("wallet must hold mnemonics");

    // 1) Mint -> m'envoie 42 (encrypté pour moi)
    let plain_mint = PlainPayload::Mint {
        outputs: vec![TxOutput { address: my_addr.clone(), amount: "42".into() }],
    };
    let enc_mint = EncryptedPayload::encrypt_for_plain(&plain_mint, &[my_xpk.clone()])
        .map_err(anyhow::Error::msg)?;

    // WireBlock utilisé seulement pour le compute_block_id
    let wb_mint = pms_wire::WireBlock {
        id: String::new(),
        parents: vec![g.id.clone()],
        payload_json: serde_json::to_string(&PayloadEnvelope::Encrypted(enc_mint.clone())).ok(),
        nonce: 1,

        network_id:       meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex:    String::new(),
        signature_hex:    String::new(),
    };
    let id_mint = compute_block_id(
        &wb_mint.parents,
        &wb_mint.payload_json.as_ref().and_then(|s| serde_json::from_str(s).ok()),
        wb_mint.nonce,
    );
    let sb_mint = StoredBlock {
        id: id_mint.clone(),
        parents: vec![g.id.clone()],
        payload_json: serde_json::to_string(&PayloadEnvelope::Encrypted(enc_mint)).ok(),
        nonce: 1,

        network_id:       meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex:    String::new(),
        signature_hex:    String::new(),
    };
    let _ = store.append_block_atomic(&sb_mint).await?;

    // 2) Tx encryptée (pour moi) mais qui n’envoie **rien** à mon adresse -> doit être filtrée
    let other_addr = "8e1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
    let tx_plain = PlainPayload::TxUtxo(Transaction {
        inputs: vec![TxInput { out: OutputId { txid: "prev".into(), index: 0 } }],
        outputs: vec![TxOutput { address: other_addr.into(), amount: "13".into() }],
        fee: "0".into(),
        unlocks: vec![Unlock { pubkey_hex: "00".into(), signature_b64: "AA==".into() }],
    });
    let enc_tx = EncryptedPayload::encrypt_for_plain(&tx_plain, &[my_xpk.clone()])
        .map_err(anyhow::Error::msg)?;

    let wb_tx = pms_wire::WireBlock {
        id: String::new(),
        parents: vec![g.id.clone()],
        payload_json: serde_json::to_string(&PayloadEnvelope::Encrypted(enc_tx.clone())).ok(),
        nonce: 2,

        network_id:       meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex:    String::new(),
        signature_hex:    String::new(),
    };
    let id_tx = compute_block_id(
        &wb_tx.parents,
        &wb_tx.payload_json.as_ref().and_then(|s| serde_json::from_str(s).ok()),
        wb_tx.nonce,
    );
    let sb_tx = StoredBlock {
        id: id_tx,
        parents: vec![g.id.clone()],
        payload_json: serde_json::to_string(&PayloadEnvelope::Encrypted(enc_tx)).ok(),
        nonce: 2,

        network_id:       meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex:    String::new(),
        signature_hex:    String::new(),
    };
    let _ = store.append_block_atomic(&sb_tx).await?;

    // Laisse la persistance respirer (principalement utile si CI lente)
    sleep(Duration::from_millis(10)).await;

    // ----- Act -----
    let dec = scan_decrypt_recent_for_address(&*store, &my_sk, &my_addr, 200).await?;

    // ----- Assert -----
    assert_eq!(dec.len(), 1, "Seul le Mint doit passer le filtre (outputs -> mon adresse)");
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