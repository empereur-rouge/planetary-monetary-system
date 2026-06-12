use pms_config::load_config;
use pms_storage::StoredBlock;
use pms_types::{BlockId, PayloadEnvelope, PlainPayload, TxOutput};
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;

pub fn test_meta_and_wallet() -> (WireMeta, Wallet) {
    let settings = load_config().expect("settings load failed");
    let meta = pms_wire::WireMeta::from(&settings);

    let wallet = Wallet::from_seed(&[7u8; 32], None).expect("wallet de test doit être créé");

    (meta, wallet)
}

// Petit utilitaire pour créer un block "valide" pour les tests
pub fn mk_block(id: &str, parents: Vec<BlockId>, meta: &WireMeta) -> StoredBlock {
    // Payload clair de type Mint
    let payload = PlainPayload::Mint {
        outputs: vec![TxOutput::new("addr", "1.0", None)],
    };

    // On stocke un PayloadEnvelope::Plain, comme dans le chemin normal
    let envelope = PayloadEnvelope::Plain(payload);

    let payload_json = Some(serde_json::to_string(&envelope).expect("serialize payload"));

    StoredBlock {
        id: id.to_string(),
        parents,
        payload_json,
        nonce: 1,

        // Nouveaux champs liés au réseau / protocole
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        // Pour ces tests-là, on ne teste pas la signature → vides
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    }
}
