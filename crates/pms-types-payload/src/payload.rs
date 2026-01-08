use crate::EncryptedPayload;
use pms_config::ConfigUpdate;
use pms_types_nft::NftAction;
use pms_types_transaction::{Transaction, TxOutput};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PayloadEnvelope {
    Plain(PlainPayload),         // DEV / interne
    Encrypted(EncryptedPayload), // PROD privé
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlainPayload {
    Genesis,
    Mint {
        outputs: Vec<TxOutput>,
    },
    TxUtxo(Transaction),
    Milestone {
        approved: Vec<String>,
        /// Si true, distribue le pool de fees aux nœuds proportionnellement à leurs blocs
        #[serde(default)]
        distribute_node_rewards: bool,
    },
    /// Action NFT (Mint, Transfer, Use, Burn)
    Nft(NftAction),
    /// Mise à jour de configuration (Coordinator seulement)
    ConfigUpdate(ConfigUpdate),
}
