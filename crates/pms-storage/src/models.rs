use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredBlock {
    pub id: String,
    pub parents: Vec<String>,
    pub payload_json: Option<String>, // PayloadEnvelope (chiffré) en JSON
    pub nonce: u64,
}