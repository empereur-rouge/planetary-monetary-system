use serde::{Serialize, Deserialize};
use pms_config::Settings;
use pms_types_block::Block;

/// Format réseau / JSON, indépendant du stockage et du core.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireBlock {
    pub id: String,
    pub parents: Vec<String>,
    pub payload_json: Option<String>,
    pub nonce: u64,

    pub network_id: String,       // ex: "devnet", "testnet", "mainnet"
    pub protocol_version: u16,    // ex: 1
    pub signer_pk_hex: String,    // clé publique (compressed) hex
    pub signature_hex: String,    // signature ECDSA hex
}

#[derive(Clone)]
pub struct WireMeta {
    pub network_id: String,
    pub protocol_version: u32,
}

impl From<WireBlock> for Block {
    fn from(wb: WireBlock) -> Self {
        Block {
            id: wb.id,
            parents: wb.parents,
            nonce: wb.nonce,
            // on convertit payload_json en Payload (si présent)
            payload: wb.payload_json
                .and_then(|s| serde_json::from_str(&s).ok()),
        }
    }
}

impl WireBlock {
    /// Bytes canoniques utilisés pour le hash + la signature
    pub fn canonical_bytes(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct Canonical<'a> {
            id: &'a str,
            parents: &'a [String],
            payload_json: &'a Option<String>,
            nonce: u64,
            network_id: &'a str,
            protocol_version: u16,
        }

        let c = Canonical {
            id: &self.id,
            parents: &self.parents,
            payload_json: &self.payload_json,
            nonce: self.nonce,
            network_id: &self.network_id,
            protocol_version: self.protocol_version,
        };

        serde_json::to_vec(&c).expect("WireBlock::canonical_bytes: serialize")
    }
}

impl From<&Settings> for WireMeta {
    fn from(s: &Settings) -> Self {
        Self {
            network_id: s.network.network_id.clone(),
            protocol_version: s.network.protocol_version,
        }
    }
}