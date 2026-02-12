// crates/pms-wallet/tests/history_encrypted_reward.rs

use anyhow::Result;
use pms_storage::models::StoredBlock;
use pms_testkit::test_rocks_store;
use pms_types_block::Block;
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_types_transaction::TxOutput;
use pms_utils::compute_block_id;
use pms_wallet::Wallet;
use pms_wallet::history::scan_decrypt_recent_for_address;
use pms_wire::WireMeta;

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
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    }
}

#[tokio::test]
async fn test_history_encrypted_reward() -> Result<()> {
    let tr = test_rocks_store("hist-reward-enc").await?;
    let store = tr.store.clone();

    let settings = pms_config::load_config()?;
    let meta = pms_wire::WireMeta::from(&settings);

    // Genesis
    let g = Block::genesis(compute_block_id);
    let gsb = mk_sb(
        g.id.clone(),
        vec![],
        g.payload.as_ref().unwrap(),
        g.nonce,
        &meta,
    );
    store.append_block_atomic(&gsb).await?;

    // Wallet (User)
    let w = Wallet::generate();
    let my_addr = w.get_address(&settings.address.hrp);
    let my_xpk = w.x25519_pub_hex.clone();
    let my_sk = w.x25519_sk_hex().unwrap();

    // 1) Create an EncryptedRewardPayload
    // We need to create EncryptedRewardOutput first.
    // In pms_types_payload, EncryptedRewardOutput { encrypted: EncryptedPayload }
    // The EncryptedPayload decrypts to TxOutput.

    let tx_out = TxOutput {
        address: my_addr.clone(),
        amount: "100".into(),
        asset_id: None,
    };
    let pt = serde_json::to_vec(&tx_out)?;

    let enc_payload = EncryptedPayload::encrypt_for(
        &pt,
        &[my_xpk.clone()], // Encrypt for me
        pt.len() as u32,
    )
    .map_err(|e| anyhow::anyhow!("{}", e))?;

    let wrap = pms_types_payload::EncryptedRewardOutput {
        encrypted: enc_payload,
    };

    let reward_payload = PlainPayload::EncryptedReward {
        encrypted_outputs: vec![wrap],
        burned: "0".into(),
        tx_block_id: "fake_tx".into(),
    };

    let env = PayloadEnvelope::Plain(reward_payload);

    // Compute ID
    let env_str = serde_json::to_string(&env).unwrap();
    let env_reparsed: PayloadEnvelope = serde_json::from_str(&env_str).unwrap();
    let id = compute_block_id(&[g.id.clone()], &Some(env_reparsed.clone()), 2);

    let sb = mk_sb(id, vec![g.id.clone()], &env_reparsed, 2, &meta);
    store.append_block_atomic(&sb).await?;

    // 2) Scan with wallet
    let dec = scan_decrypt_recent_for_address(&store, &my_sk, &my_addr, 100).await?;

    // 3) Verify
    assert_eq!(dec.len(), 1);
    match &dec[0].plain {
        PlainPayload::Reward { reward_outputs, .. } => {
            assert_eq!(reward_outputs.len(), 1);
            assert_eq!(reward_outputs[0].address, my_addr);
            assert_eq!(reward_outputs[0].amount, "100");
        }
        _ => panic!("Expected decrypted Reward payload"),
    }

    Ok(())
}
