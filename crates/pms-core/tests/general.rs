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
    let genesis = Block::genesis(compute_block_id);
    let genesis_id = genesis.id.clone();
    let mut dag = Dag::new_with_genesis(genesis);

    // 3 blocs Mint, chacun avec une adresse en CLAIR dans le payload, puis
    // CHIFFRÉ. L'export ne doit jamais laisser fuiter ces adresses.
    let secret_addrs: Vec<String> = (0..3).map(|i| format!("SECRET_addr_{i}")).collect();
    for addr in &secret_addrs {
        let mint_block = PlainPayload::Mint {
            outputs: vec![TxOutput {
                address: addr.clone(),
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
    let pretty = serde_json::to_string_pretty(&j).unwrap();
    println!("=== BACKUP ===\n{pretty}");

    // 1) Le dump contient exactement 4 blocs : genesis + 3 Mint chiffrés.
    let blocks = j["blocks"].as_array().expect("export must have a blocks array");
    println!("exported {} blocks", blocks.len());
    assert_eq!(blocks.len(), 4, "genesis + 3 mints expected");

    // 2) Le genesis est présent par son id réel.
    assert!(
        blocks.iter().any(|b| b["id"] == serde_json::json!(genesis_id)),
        "genesis block id {genesis_id} must appear in the export"
    );

    // 3) Les 3 blocs chiffrés exposent un ciphertext non vide + des destinataires,
    //    et JAMAIS le plaintext en clair.
    let encrypted: Vec<&serde_json::Value> = blocks
        .iter()
        .filter(|b| b["payload"].get("ciphertext_b64").is_some())
        .collect();
    assert_eq!(encrypted.len(), 3, "the 3 mint blocks must be encrypted");
    for b in &encrypted {
        let ct = b["payload"]["ciphertext_b64"].as_str().unwrap_or("");
        assert!(!ct.is_empty(), "ciphertext must not be empty");
        assert!(
            b["payload"]["recipients"]
                .as_array()
                .map(|r| !r.is_empty())
                .unwrap_or(false),
            "encrypted payload must list at least one recipient"
        );
        assert!(
            b["payload"].get("scheme").is_some(),
            "encrypted payload must carry its scheme"
        );
    }

    // 4) ANTI-FUITE : aucune adresse en clair ne doit survivre dans le dump.
    for addr in &secret_addrs {
        assert!(
            !pretty.contains(addr.as_str()),
            "cleartext address {addr} leaked into the encrypted DAG export"
        );
    }

    // 5) Round-trip : le JSON exporté se re-sérialise/désérialise sans perte de blocs.
    let reparsed: serde_json::Value = serde_json::from_str(&pretty).unwrap();
    assert_eq!(
        reparsed["blocks"].as_array().unwrap().len(),
        4,
        "round-trip must preserve all 4 blocks"
    );
}
