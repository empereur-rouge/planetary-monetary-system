use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptedPayload {
    pub scheme: String,           // "x25519+chacha20poly1305"
    pub key_version: u32,         // rotation de clé
    pub aad: AAD,                 // données publiques liées (auth)
    pub commitment: String,       // hash(payload clair) pour intégrité
    pub ciphertext_b64: String,   // corps chiffré
    pub recipients: Vec<KeyWrap>, // enveloppe de clé symm par destinataire
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AAD {                 // données authentifiées MAIS publiques
    pub payload_type: String,     // "TxUtxo" | "Reward" | "Milestone" | "NFT"
    pub len_hint: u32,            // padding possible
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyWrap {             // ECIES/X25519 → clé symm chiffrée
    pub recipient_pub: String,    // hex
    pub wrapped_key_b64: String,
}