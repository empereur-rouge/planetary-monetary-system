use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type TxId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transaction {
    pub inputs: Vec<TxInput>,
    pub outputs: Vec<TxOutput>,
    pub fee: String,          // String pour compatibilité décimale
    pub unlocks: Vec<Unlock>, // signatures
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxInput {
    pub out: OutputId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxOutput {
    pub address: String,
    pub amount: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct OutputId {
    pub txid: TxId,
    pub index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Unlock {
    pub pubkey_hex: String,
    pub signature_b64: String,
}

impl Transaction {
    pub fn signing_message(&self) -> anyhow::Result<String> {
        #[derive(Serialize)]
        struct Canon<'a> {
            inputs: &'a [crate::TxInput],
            outputs: &'a [crate::TxOutput],
            fee: &'a str,
        }
        let canon = Canon {
            inputs: &self.inputs,
            outputs: &self.outputs,
            fee: &self.fee,
        };
        let bytes = serde_json::to_vec(&canon)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}
