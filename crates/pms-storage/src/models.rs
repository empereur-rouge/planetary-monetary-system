use serde::{Serialize, Deserialize};
use pms_wire::WireBlock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredBlock {
    pub id: String,
    pub parents: Vec<String>,
    pub payload_json: Option<String>, // PayloadEnvelope (chiffré) en JSON
    pub nonce: u64,
    pub network_id: String,
    pub protocol_version: u16,
    pub signer_pk_hex: String,
    pub signature_hex: String,
}

// === Conversions simples ===

impl From<WireBlock> for StoredBlock {
    fn from(w: WireBlock) -> Self {
        StoredBlock {
            id: w.id,
            parents: w.parents,
            payload_json: w.payload_json,
            nonce: w.nonce,
            network_id: w.network_id,
            protocol_version: w.protocol_version as u16,
            signer_pk_hex: w.signer_pk_hex,
            signature_hex: w.signature_hex,
        }
    }
}

impl From<StoredBlock> for WireBlock {
    fn from(s: StoredBlock) -> Self {
        WireBlock {
            id: s.id,
            parents: s.parents,
            payload_json: s.payload_json,
            nonce: s.nonce,
            network_id: s.network_id,
            protocol_version: s.protocol_version as u16,
            signer_pk_hex: s.signer_pk_hex,
            signature_hex: s.signature_hex,
        }
    }
}