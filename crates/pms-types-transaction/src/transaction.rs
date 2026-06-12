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
    /// Condition de déverrouillage (protocole 2.2), portée par l'output
    /// (pattern Bitcoin scriptPubKey). `None` = [`SpendCondition::PubKey`]
    /// (binding C-1 classique : 1 signature du propriétaire de l'adresse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend_condition: Option<SpendCondition>,
    /// Timestamp de création de l'UTXO (UNIX ms) — **assigné par le système**
    /// au moment du persist du bloc, base du calcul de demurrage (2.5).
    ///
    /// ⚠️ Toute valeur fournie par le client dans une transaction est ÉCRASÉE
    /// par le pipeline de persistance (anti-antidatage). Les clients doivent
    /// laisser `None` (le champ est alors absent du message signé).
    /// `None` en lecture = UTXO pré-v0.10.0 → aucun demurrage ne court
    /// (la décote démarre au premier mouvement post-upgrade).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
}

/// Condition de déverrouillage d'un output (protocole 2.2).
///
/// La condition est figée À LA CRÉATION de l'output et vit on-DAG : au moment
/// de la dépense, le validateur lit la condition depuis l'UTXO STOCKÉ (jamais
/// depuis les données fournies par le dépensier).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SpendCondition {
    /// Défaut historique : une signature dont la pubkey dérive l'adresse de
    /// l'output (audit C-1). Équivalent à `spend_condition: None`.
    PubKey,
    /// M-sur-N : au moins `m` signatures valides parmi `pubkeys` (secp256k1
    /// hex). L'adresse de l'output DOIT être l'adresse multisig canonique
    /// dérivée de la policy (voir `multisig_address` dans pms-core) — le set
    /// de clés est donc engagé par l'adresse elle-même.
    MultiSig { m: u8, pubkeys: Vec<String> },
    /// Hash-lock : la dépense doit révéler `preimage` tel que
    /// `SHA256(preimage) == hash_hex` (64 hex chars). La signature de la tx
    /// reste obligatoire (intégrité), mais N'IMPORTE quelle clé peut signer —
    /// le secret EST l'autorisation.
    HashLock { hash_hex: String },
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
            spend_condition: None,
            created_at: None,
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
    /// Signatures additionnelles pour un input sous condition
    /// [`SpendCondition::MultiSig`] : la paire principale
    /// (`pubkey_hex`/`signature_b64`) compte comme première signature, les
    /// cosignataires complètent jusqu'à M. Chaque cosignature porte sur le
    /// MÊME message canonique `{network_id, inputs, outputs, fee}`.
    ///
    /// `Unlock` n'entre pas dans `signing_message` → ajouter des cosignatures
    /// ne change pas le message signé (pas de cycle signature↔contenu).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cosigners: Vec<Cosigner>,
    /// Préimage hex pour un input sous condition [`SpendCondition::HashLock`] :
    /// `SHA256(hex::decode(preimage_hex)) == hash_hex` de la condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preimage_hex: Option<String>,
}

/// Signature additionnelle d'un cosignataire MultiSig (voir [`Unlock::cosigners`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cosigner {
    pub pubkey_hex: String,
    pub signature_b64: String,
}

impl Unlock {
    /// Constructeur canonique d'un unlock « simple » (1 signature, pas de
    /// condition spéciale). Les champs optionnels (cosigners, preimage)
    /// futurs/présents sont initialisés vides.
    pub fn new(pubkey_hex: impl Into<String>, signature_b64: impl Into<String>) -> Self {
        Self {
            pubkey_hex: pubkey_hex.into(),
            signature_b64: signature_b64.into(),
            cosigners: Vec::new(),
            preimage_hex: None,
        }
    }
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
