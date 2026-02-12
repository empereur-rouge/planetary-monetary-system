use pms_core::Dag;
use pms_types::{Block, EncryptedPayload, PayloadEnvelope, PlainPayload, TxOutput};
use pms_utils::compute_block_id;
use rand::RngCore;
use x25519_dalek::{PublicKey, StaticSecret};

fn gen_keypair_hex() -> (String, String) {
    let mut sk_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut sk_bytes);
    let sk = StaticSecret::from(sk_bytes);
    let pk = PublicKey::from(&sk);
    (hex::encode(sk.to_bytes()), hex::encode(pk.to_bytes()))
}

#[test]
fn test_export_dag_json() {
    let mut dag = Dag::new_with_genesis(Block::genesis(compute_block_id));

    // ajoute quelques blocks bidons
    for i in 0..3 {
        let mint_block = PlainPayload::Mint {
            outputs: vec![TxOutput {
                address: format!("addr{i}"),
                amount: "10.0".into(),
                asset_id: None,
            }],
        };
        let pt = serde_json::to_vec(&mint_block).unwrap();

        let (_sk_hex, pk_hex) = gen_keypair_hex();
        let enc = EncryptedPayload::encrypt_for(&pt, &[pk_hex], pt.len() as u32).unwrap();

        dag.add_payload_auto_parents_mined(
            Some(PayloadEnvelope::Encrypted(enc)),
            1,
            compute_block_id,
        )
        .unwrap();
    }

    let j = dag.export_json();
    println!(
        "=== BACKUP ===\n{}",
        serde_json::to_string_pretty(&j).unwrap()
    );
}
