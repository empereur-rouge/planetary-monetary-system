use aes_gcm::aead::OsRng;
use pms_types_payload::{EncryptedPayload, PlainPayload};
use pms_types_transaction::TxOutput;
use x25519_dalek::{PublicKey as XPublic, StaticSecret as XSecret};

#[test]
fn mvp_encrypt_decrypt_confidential_type_only() {
    // 1) Génère la paire X25519
    let sk = XSecret::random_from_rng(OsRng);
    let pk = XPublic::from(&sk);
    let sk_hex = hex::encode(sk.to_bytes());
    let pk_hex = hex::encode(pk.to_bytes());

    // 2) Payload clair
    let mint_block = PlainPayload::Mint {
        outputs: vec![TxOutput {
            address: "wallet123".to_string(),
            amount: "42.00000000".to_string(),
            asset_id: None,
        }],
    };
    let pt = serde_json::to_vec(&mint_block).unwrap();

    // 3) Chiffrement
    let enc = EncryptedPayload::encrypt_for(&pt, &vec![pk_hex], pt.len() as u32).unwrap();

    // 4) Déchiffrement (type confidentiel → seulement après decrypt)
    let back = enc.decrypt_as_payload(&sk_hex).unwrap();

    // 5) Print uniquement le type
    match back {
        PlainPayload::Genesis => println!("Type = Genesis"),
        PlainPayload::Mint { .. } => println!("Type = Mint"),
        PlainPayload::TxUtxo(_) => println!("Type = TxUtxo"),
        PlainPayload::Milestone { .. } => println!("Type = Milestone"),
        PlainPayload::Nft(_) => println!("Type = Nft"),
        PlainPayload::ConfigUpdate(_) => println!("Type = ConfigUpdate"),
        PlainPayload::Reward { .. } => println!("Type = Reward"),
        PlainPayload::EncryptedReward { .. } => println!("Type = EncryptedReward"),
        PlainPayload::TokenCreate(_) => println!("Type = TokenCreate"),
    }

    // Vérif : bien du bon type
    assert!(matches!(back, PlainPayload::Mint { .. }));
}
