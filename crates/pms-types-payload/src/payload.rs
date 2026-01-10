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

/// Un output chiffré individuellement pour son destinataire + coordinateur
/// Contient un TxOutput (address, amount) chiffré
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptedRewardOutput {
    /// Le payload chiffré contenant {address, amount}
    /// Recipients: [destinataire_x25519, coordinator_x25519]
    pub encrypted: EncryptedPayload,
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
    /// Distribution de récompenses (fees + block rewards) - VERSION PLAIN (dev only)
    /// Créé automatiquement par le serveur après chaque transaction
    Reward {
        /// Outputs de distribution des fees (treasury, creator, parents)
        fee_outputs: Vec<TxOutput>,
        /// Outputs de block reward (creator, treasury)
        reward_outputs: Vec<TxOutput>,
        /// Montant brûlé (deflationary)
        #[serde(default)]
        burned: String,
        /// ID du bloc de transaction associé
        tx_block_id: String,
    },
    /// Distribution de récompenses CHIFFRÉES (pour production)
    /// Chaque output est chiffré individuellement pour son destinataire + coordinator
    EncryptedReward {
        /// Outputs chiffrés individuellement
        encrypted_outputs: Vec<EncryptedRewardOutput>,
        /// Montant brûlé (public, pas de destinataire)
        #[serde(default)]
        burned: String,
        /// ID du bloc de transaction associé
        tx_block_id: String,
    },
}
