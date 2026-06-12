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
    /// None = PMS natif. Some("edenite") = token custom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    /// Time-lock natif (protocole 2.1) : l'output est indépensable tant que
    /// l'horloge du validateur n'a pas atteint ce timestamp **UNIX en
    /// millisecondes**. `None` = dépensable immédiatement (comportement
    /// historique).
    ///
    /// Rétro-compat sérialisation : `skip_serializing_if` garantit qu'un
    /// output sans lock produit exactement le même JSON canonique qu'avant —
    /// le `signing_message` des transactions existantes est inchangé.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_until: Option<u64>,
}

impl TxOutput {
    /// Constructeur canonique d'un output « simple » (sans champ protocole
    /// optionnel). À préférer à la construction littérale dans les tests et
    /// helpers : les futurs champs optionnels de `TxOutput` (time-lock,
    /// spend condition, …) seront initialisés à `None` ici sans casser les
    /// call sites.
    pub fn new(
        address: impl Into<String>,
        amount: impl Into<String>,
        asset_id: Option<String>,
    ) -> Self {
        Self {
            address: address.into(),
            amount: amount.into(),
            asset_id,
            locked_until: None,
        }
    }

    /// Variante de [`TxOutput::new`] avec un time-lock (timestamp UNIX ms).
    pub fn new_locked(
        address: impl Into<String>,
        amount: impl Into<String>,
        asset_id: Option<String>,
        locked_until: u64,
    ) -> Self {
        Self {
            locked_until: Some(locked_until),
            ..Self::new(address, amount, asset_id)
        }
    }
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
    /// Canonical signing message — bound to a specific `network_id` to prevent
    /// cross-chain replay (a TX signed for testnet must not validate on mainnet).
    /// Returns the SHA-256 of the canonical JSON `{network_id, inputs, outputs, fee}`,
    /// hex-encoded.
    pub fn signing_message(&self, network_id: &str) -> anyhow::Result<String> {
        #[derive(Serialize)]
        struct Canon<'a> {
            network_id: &'a str,
            inputs: &'a [crate::TxInput],
            outputs: &'a [crate::TxOutput],
            fee: &'a str,
        }
        let canon = Canon {
            network_id,
            inputs: &self.inputs,
            outputs: &self.outputs,
            fee: &self.fee,
        };
        let bytes = serde_json::to_vec(&canon)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}
