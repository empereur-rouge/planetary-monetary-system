use crate::EncryptedPayload;
use pms_config::ConfigUpdate;
use pms_types_contract::Contract;
use pms_types_nft::NftAction;
use pms_types_transaction::{Transaction, TxInput, TxOutput};
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
    /// Enregistrement d'un nouveau token (Coordinator seulement)
    TokenCreate(TokenMetadata),
    /// Verrouille des UTXOs sur ce ledger pour un transfert cross-ledger.
    /// Les fonds sont détruits sur le ledger source. Coordinator seulement.
    BridgeLock {
        /// UTXOs consommés (même format que TxUtxo inputs)
        inputs: Vec<TxInput>,
        /// Montant total verrouillé
        amount: String,
        /// Asset transféré (None = PMS natif)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asset_id: Option<String>,
        /// ID du ledger destination
        dest_ledger_id: String,
        /// Adresse du destinataire sur le ledger destination
        dest_address: String,
    },
    /// Crée des UTXOs sur ce ledger en référençant un BridgeLock source.
    /// Coordinator seulement.
    BridgeMint {
        /// Outputs créés sur ce ledger
        outputs: Vec<TxOutput>,
        /// ID du bloc BridgeLock sur le ledger source (preuve)
        lock_block_id: String,
        /// ID du ledger source
        source_ledger_id: String,
    },
    /// Gèle un compte : bloque toutes les transactions entrantes et sortantes.
    /// Coordinator seulement. Réversible via Unfreeze.
    Freeze {
        address: String,
        reason: String,
    },
    /// Dégèle un compte précédemment gelé. Coordinator seulement.
    Unfreeze {
        address: String,
        reason: String,
        freeze_block_id: String,
    },
    /// Saisit des UTXOs et les transfère au treasury. Coordinator seulement.
    Seize {
        from_address: String,
        inputs: Vec<TxInput>,
        outputs: Vec<TxOutput>,
        reason: String,
    },
    /// Inverse une transaction si ses outputs n'ont pas été dépensés.
    /// Coordinator seulement.
    Reverse {
        original_block_id: String,
        inputs: Vec<TxInput>,
        outputs: Vec<TxOutput>,
        reason: String,
    },
    /// Enregistrement d'un contrat déclaratif. Coordinator seulement.
    /// Le contrat est stocké dans RocksDB et évalué par le ContractEngine.
    ContractRegister(Contract),
    /// Activation/désactivation d'un contrat existant. Coordinator seulement.
    ContractUpdate {
        contract_id: String,
        enabled: bool,
        reason: String,
    },
}

/// Métadonnées d'un token enregistré dans le DAG.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenMetadata {
    /// Identifiant unique du token (ex: "edenite")
    pub asset_id: String,
    /// Symbole court (ex: "EDEN")
    pub symbol: String,
    /// Nom complet (ex: "Edenite Token")
    pub name: String,
    /// Nombre de décimales (ex: 8)
    pub decimals: u8,
    /// Supply maximum (None = illimité)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_supply: Option<String>,
    /// Adresse du créateur
    pub creator: String,
    /// Clé publique autorisée à mint ce token
    pub mint_authority: String,
}
