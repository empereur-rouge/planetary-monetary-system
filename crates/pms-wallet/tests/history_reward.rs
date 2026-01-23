// crates/pms-wallet/tests/history_reward.rs

use anyhow::Result;
use pms_storage::models::StoredBlock;
use pms_testkit::test_rocks_store;
use pms_types_block::Block;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_types_transaction::TxOutput;
use pms_utils::compute_block_id;
use pms_wallet::Wallet;
use pms_wallet::history::history_plain_for_address;
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
async fn test_history_reward_plain() -> Result<()> {
    let tr = test_rocks_store("hist-reward").await?;
    let store = tr.store.clone();

    let settings = pms_config::load_config()?;
    let meta = pms_wire::WireMeta::from(&settings);

    let g = Block::genesis(compute_block_id);
    let gsb = StoredBlock {
        id: g.id.clone(),
        parents: vec![],
        payload_json: serde_json::to_string(&g.payload).ok(),
        nonce: g.nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };
    store.append_block_atomic(&gsb).await?;

    let w = Wallet::generate();
    let my_addr = w.get_address(&settings.address.hrp);

    // 1) Create a Reward payload where my_addr is in fee_outputs
    let reward_payload = PlainPayload::Reward {
        fee_outputs: vec![TxOutput {
            address: my_addr.clone(),
            amount: "10".into(),
        }],
        reward_outputs: vec![],
        burned: "0".into(),
        tx_block_id: "some_tx".into(),
    };

    let env = PayloadEnvelope::Plain(reward_payload.clone());

    // We recreate env from string to match exactly what happens in compute_block_id internally potentially
    let env_str = serde_json::to_string(&env).unwrap();
    let env_reparsed: PayloadEnvelope = serde_json::from_str(&env_str).unwrap();

    let id = compute_block_id(&[g.id.clone()], &Some(env_reparsed), 1);

    let sb = mk_sb(id, vec![g.id.clone()], &env, 1, &meta);
    store.append_block_atomic(&sb).await?;

    // 2) Query history
    let history = history_plain_for_address(&store, &my_addr, 10).await?;

    // 3) Verify
    assert_eq!(history.len(), 1);
    match &history[0].plain {
        PlainPayload::Reward { fee_outputs, .. } => {
            assert_eq!(fee_outputs.len(), 1);
            assert_eq!(fee_outputs[0].address, my_addr);
        }
        _ => panic!("Expected Reward payload"),
    }

    Ok(())
}
