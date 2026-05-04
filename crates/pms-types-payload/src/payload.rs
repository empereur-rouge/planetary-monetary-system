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
    /// Transfert d'ownership d'un ledger, enregistré dans le DAG pour traçabilité.
    ///
    /// Le contenu sensible (new_owner_pubkey) est chiffré via X25519+AES-256-GCM.
    /// Seuls le propriétaire actuel et le coordinateur peuvent le déchiffrer.
    /// Le `ledger_id` reste en clair pour le routage et la validation.
    ///
    /// # Sécurité
    /// - Coordinator seulement (signature requise).
    /// - Le bloc constitue une preuve immuable du changement d'ownership dans le DAG.
    /// - Conforme à la règle : toute mutation d'état DOIT passer par le DAG.
    LedgerOwnershipTransfer {
        /// ID du ledger concerné (cleartext — nécessaire pour validation et routage).
        ledger_id: String,
        /// Données de transfert chiffrées : `OwnershipTransferData` sérialisé en JSON.
        /// Recipients: \[owner_x25519 (si connu), coordinator_x25519\]
        encrypted_transfer: EncryptedPayload,
    },
    /// Rotation de la clé Coordinator (audit item 8, v0.7.4).
    ///
    /// Permet de remplacer la clé secp256k1 qui signe les blocs sans
    /// arrêter le réseau. Le bloc DOIT être signé par `old_pk`, qui
    /// DOIT être la clé Coordinator courante au moment du persist
    /// (bootstrap config OU dernier `new_pk` d'une rotation antérieure
    /// qui a déjà été appliquée). Une fois persisté :
    ///   - `new_pk` devient la clé "courante".
    ///   - `old_pk` reste un signataire valide pendant
    ///     `grace_window_seconds` secondes après le timestamp du bloc
    ///     (si `0`, la révocation est immédiate / atomic rotation).
    ///
    /// Le bloc est tracé dans le CF `coordinator_key_history` et
    /// rejoué au boot pour reconstruire l'ensemble des clés acceptées.
    CoordinatorKeyRotate {
        /// Clé qui signe ce bloc — DOIT être la coordinator key courante.
        old_pk: String,
        /// Nouvelle clé Coordinator qui prendra le relais.
        new_pk: String,
        /// Fenêtre de tolérance pendant laquelle `old_pk` reste valide.
        /// `0` révoque l'ancienne clé immédiatement après ce bloc.
        grace_window_seconds: u64,
    },
}

/// Données de transfert d'ownership d'un ledger, sérialisées en JSON
/// puis chiffrées dans le champ `encrypted_transfer` de `LedgerOwnershipTransfer`.
///
/// Seuls le propriétaire actuel et le coordinateur peuvent déchiffrer ces données.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnershipTransferData {
    /// Nouvelle clé publique du propriétaire.
    /// `None` = retour à admin-owned (pas de propriétaire spécifique).
    pub new_owner_pubkey: Option<String>,
    /// Raison du transfert (audit trail).
    pub reason: String,
}

impl PayloadEnvelope {
    /// True iff the payload is wrapped in (or itself wraps) an encrypted
    /// envelope the server cannot decrypt. Used by webhook delivery and
    /// the multi-address SSE to flag events whose detail the server can't
    /// surface — the SaaS must use the per-wallet activity stream
    /// (which has the recipient X25519 key) for full detail.
    pub fn is_encrypted(&self) -> bool {
        matches!(
            self,
            PayloadEnvelope::Encrypted(_)
                | PayloadEnvelope::Plain(PlainPayload::EncryptedReward { .. })
        )
    }
}

impl PlainPayload {
    /// Returns the outputs created by this payload, in the same order as
    /// they're indexed when forming `OutputId.index`. Used by every site
    /// that needs to resolve a UTXO to its `(address, amount, asset_id)` —
    /// `transaction_lookup`, UTXO indexer at persist time, balance scanner.
    /// Returns `None` for payload variants that create no UTXOs (Milestone,
    /// ConfigUpdate, ContractRegister, …).
    ///
    /// **Reward** : `fee_outputs` come first, then `reward_outputs` —
    /// matches how `persist_block` builds the `UtxoDelta`. Don't reorder
    /// without auditing every caller (UTXO indexing, lookup, balance).
    ///
    /// **Seize / Reverse** : returned even though they're coordinator-only
    /// — payment-rail watchers may need to display compliance reversals.
    pub fn outputs(&self) -> Option<Vec<TxOutput>> {
        match self {
            PlainPayload::TxUtxo(t) => Some(t.outputs.clone()),
            PlainPayload::Mint { outputs } => Some(outputs.clone()),
            PlainPayload::Reward {
                fee_outputs,
                reward_outputs,
                ..
            } => {
                let mut all = fee_outputs.clone();
                all.extend(reward_outputs.clone());
                Some(all)
            }
            PlainPayload::BridgeMint { outputs, .. } => Some(outputs.clone()),
            PlainPayload::Seize { outputs, .. } => Some(outputs.clone()),
            PlainPayload::Reverse { outputs, .. } => Some(outputs.clone()),
            // No UTXO creation: Genesis, Milestone, ConfigUpdate, EncryptedReward,
            // TokenCreate, BridgeLock, Freeze, Unfreeze, ContractRegister,
            // ContractUpdate, LedgerOwnershipTransfer, CoordinatorKeyRotate, Nft.
            _ => None,
        }
    }
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
