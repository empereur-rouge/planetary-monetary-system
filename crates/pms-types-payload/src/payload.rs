use crate::EncryptedPayload;
use pms_types_transaction::{Transaction, TxOutput};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PayloadEnvelope {
    Plain(PlainPayload),         // DEV / interne
    Encrypted(EncryptedPayload), // PROD privé
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlainPayload {
    // comme avant (UTXO, Mint, Milestone…)
    Genesis,
    Mint { outputs: Vec<TxOutput> },
    TxUtxo(Transaction),
    Milestone { approved: Vec<String> },
}
