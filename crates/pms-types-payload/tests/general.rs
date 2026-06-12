use aes_gcm::aead::OsRng;
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_types_transaction::{TxInput, TxOutput};
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
        outputs: vec![TxOutput::new("wallet123".to_string(), "42.00000000".to_string(), None)],
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
        PlainPayload::BridgeLock { .. } => println!("Type = BridgeLock"),
        PlainPayload::BridgeMint { .. } => println!("Type = BridgeMint"),
        PlainPayload::Freeze { .. } => println!("Type = Freeze"),
        PlainPayload::Unfreeze { .. } => println!("Type = Unfreeze"),
        PlainPayload::Seize { .. } => println!("Type = Seize"),
        PlainPayload::Reverse { .. } => println!("Type = Reverse"),
        PlainPayload::ContractRegister(_) => println!("Type = ContractRegister"),
        PlainPayload::ContractUpdate { .. } => println!("Type = ContractUpdate"),
        PlainPayload::LedgerOwnershipTransfer { .. } => println!("Type = LedgerOwnershipTransfer"),
        PlainPayload::CoordinatorKeyRotate { .. } => println!("Type = CoordinatorKeyRotate"),
        PlainPayload::ReserveSnapshot { .. } => println!("Type = ReserveSnapshot"),
    }

    // Vérif : bien du bon type
    assert!(matches!(back, PlainPayload::Mint { .. }));
}

// ── Roundtrip serde tests for compliance variants ─────────────────────────

#[test]
fn serde_roundtrip_freeze() {
    let payload = PlainPayload::Freeze {
        address: "8e1addr_test".into(),
        reason: "suspicious activity".into(),
    };
    let envelope = PayloadEnvelope::Plain(payload.clone());
    let json = serde_json::to_string(&envelope).unwrap();
    let back: PayloadEnvelope = serde_json::from_str(&json).unwrap();
    assert_eq!(back, envelope);
    assert!(matches!(
        back,
        PayloadEnvelope::Plain(PlainPayload::Freeze { .. })
    ));
}

#[test]
fn serde_roundtrip_unfreeze() {
    let payload = PlainPayload::Unfreeze {
        address: "8e1addr_test".into(),
        reason: "investigation complete".into(),
        freeze_block_id: "abc123def456".into(),
    };
    let envelope = PayloadEnvelope::Plain(payload);
    let json = serde_json::to_string(&envelope).unwrap();
    let back: PayloadEnvelope = serde_json::from_str(&json).unwrap();
    assert_eq!(back, envelope);
}

#[test]
fn serde_roundtrip_seize() {
    let payload = PlainPayload::Seize {
        from_address: "8e1target_addr".into(),
        inputs: vec![TxInput {
            out: pms_types_transaction::OutputId {
                txid: "tx123".into(),
                index: 0,
            },
        }],
        outputs: vec![TxOutput::new("8e1treasury", "500.0", None)],
        reason: "court order".into(),
    };
    let envelope = PayloadEnvelope::Plain(payload);
    let json = serde_json::to_string(&envelope).unwrap();
    let back: PayloadEnvelope = serde_json::from_str(&json).unwrap();
    assert_eq!(back, envelope);
}

#[test]
fn serde_roundtrip_reverse() {
    let payload = PlainPayload::Reverse {
        original_block_id: "block_abc123".into(),
        inputs: vec![TxInput {
            out: pms_types_transaction::OutputId {
                txid: "block_abc123".into(),
                index: 0,
            },
        }],
        outputs: vec![TxOutput::new("8e1original_sender", "100.0", None)],
        reason: "fraud detected".into(),
    };
    let envelope = PayloadEnvelope::Plain(payload);
    let json = serde_json::to_string(&envelope).unwrap();
    let back: PayloadEnvelope = serde_json::from_str(&json).unwrap();
    assert_eq!(back, envelope);
}
