use crate::EncryptedPayload;
use pms_config::{ConfigUpdate, GovernanceTier};
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
    /// Brûle des tokens : destruction permanente (la supply baisse). Le burner
    /// dépense ses UTXOs via `tx.inputs` (signés par ses `tx.unlocks`, comme
    /// `TxUtxo`) ; `tx.outputs` est le change (même asset, renvoyé à `owner`).
    /// `amount = Σ(inputs[asset_id]) − Σ(outputs[asset_id])` est DÉTRUIT (aucun
    /// output créé pour cette part → la supply baisse).
    ///
    /// **Owner-signé** (l'utilisateur brûle ses propres fonds — autorité comme
    /// `TxUtxo`, pas coordinator-only ; le bloc reste forgé/signé par le
    /// Coordinator en single-writer).
    ///
    /// **Payload PLAIN par choix** (pas chiffré comme les transferts) : un burn
    /// est public-by-design (proof-of-burn auditable, supply vérifiable), comme
    /// `BridgeLock`/`Seize`. Conséquence assumée : l'adresse du burner et le
    /// montant sont en clair on-DAG.
    ///
    /// **Destiné à déclencher les contrats `OnTokenBurn{asset_id}`** — le
    /// câblage event→listener→mint (voie B complète) arrive en phase suivante ;
    /// aujourd'hui le burn est inerte côté contrats (aucun mint déclenché).
    TokenBurn {
        /// Transaction portant les `inputs` (UTXOs consommés), le change
        /// (`outputs`), et les `unlocks` (signatures du owner).
        ///
        /// **INVARIANTS** (vérifiés par `validate_token_burn_async`) : `tx.fee`
        /// DOIT être `"0"` (un burn ne paie pas de frais) ; `tx.outputs` est
        /// UNIQUEMENT du change vers `owner` (pas de tiers, même `asset_id`).
        /// Ces contraintes sont structurellement absentes du type `Transaction`
        /// réutilisé — elles sont imposées à la validation, pas par le schéma.
        tx: Transaction,
        /// Asset brûlé (`None` = PMS natif). **Dérivation épinglée par le
        /// validateur** (cf. ci-dessous).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asset_id: Option<String>,
        /// Montant détruit (= Σ inputs − Σ change, pour `asset_id`). Réduit la supply.
        amount: String,
        /// Adresse du burner (routage + trigger contrat + activité).
        ///
        /// **`owner`, `amount`, `asset_id` ne sont PAS couverts par la signature
        /// du burner** (`signing_message` ne commit que `{network_id, inputs,
        /// outputs, fee}`). Ce sont des dénormalisations de commodité (les
        /// chemins sans accès UTXO — classify, block_id — en ont besoin) que
        /// `validate_token_burn_async` RE-DÉRIVE et ÉPINGLE contre l'état signé +
        /// le UTXO set (tous les inputs/change appartiennent à `owner`, même
        /// asset, `amount == inputs − change`). Sûr UNIQUEMENT grâce à ce
        /// re-check ; ne jamais l'affaiblir.
        owner: String,
    },
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
    /// Proposition de gouvernance (plan §4) : **annonce** un changement de config
    /// timelocké, ancré DAG. N'applique RIEN — le timelock court jusqu'à
    /// `enact_after_ms` (= `announced_at_ms + durée(tier)`). Coordinator-only.
    /// Le changement réel est appliqué par un `GovernanceEnact` après expiration.
    GovernanceProposal {
        /// `SHA-256(update + tier + announced_at)` — déterministe.
        proposal_id: String,
        /// Le changement de config à appliquer à l'enact.
        update: ConfigUpdate,
        /// Palier d'impact (→ durée du timelock).
        tier: GovernanceTier,
        /// Justification publique.
        reason: String,
        /// Horodatage de l'annonce (ms).
        announced_at_ms: u64,
        /// Effet autorisé à partir de cet instant (ms). **Invariant timelock**.
        enact_after_ms: u64,
    },
    /// **Applique** une proposition après expiration du timelock — exécute son
    /// `ConfigUpdate`. REJETÉ si `now < enact_after` (timelock non écoulé) ou si
    /// la proposition n'est pas `Pending`. Coordinator-only.
    GovernanceEnact {
        proposal_id: String,
        reason: String,
    },
    /// **Annule** une proposition `Pending` (avant effet). Coordinator-only.
    GovernanceCancel {
        proposal_id: String,
        reason: String,
    },
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
    /// Enregistrement d'une nouvelle **classe semi-fongible** (SFT, façon
    /// ERC-1155 — `pms-spec-semi-fungibles.md`). Coordinator seulement. La classe
    /// est un asset fongible (`asset_id = "collection:class"`) porté par le moteur
    /// UTXO, avec des métadonnées riches PUBLIQUES. Mint/transfert/burn réutilisent
    /// `Mint`/`TxUtxo`/`TokenBurn`.
    SftClassCreate(SftClass),
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
    /// Preuve de réserves ancrée (protocole 2.6) — snapshot périodique de
    /// l'état agrégé du ledger, signé Coordinator.
    ///
    /// `state_root` = SHA-256 de l'itération ORDONNÉE (ordre des clés
    /// RocksDB, vue point-in-time consistante) de tous les UTXOs non dépensés
    /// (clé + valeur stockée), domain-séparé `pms-reserves-v1`. Tout
    /// vérificateur disposant du même état peut recomputer le root et
    /// comparer ; un mismatch prouve une divergence d'état.
    ///
    /// Le bloc constitue la preuve immuable on-DAG (conformément à la règle :
    /// toute donnée d'audit passe par le DAG) ; un pointeur de commodité vers
    /// le dernier snapshot est indexé hors-DAG pour `GET /v1/reserves/latest`.
    ReserveSnapshot {
        /// Racine d'état des UTXOs (64 hex chars, SHA-256).
        state_root: String,
        /// Supply totale par asset au moment du snapshot :
        /// `(asset_id, montant)` — `None` = natif du ledger.
        total_supply: Vec<(Option<String>, String)>,
        /// Nombre d'UTXOs non dépensés couverts par le root.
        utxo_count: u64,
        /// Horodatage du calcul (UNIX ms), informatif.
        computed_at_ms: u64,
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
            // TokenBurn: only the CHANGE outputs are created UTXOs; the burned
            // amount creates nothing (supply drops). Indexer/balance must see change.
            PlainPayload::TokenBurn { tx, .. } => Some(tx.outputs.clone()),
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
    /// Demurrage opt-in (protocole 2.5) : décote en basis points par JOUR
    /// PLEIN écoulé depuis la création de l'UTXO (`floor((now - created_at)
    /// / 24h)`). `None` ou `0` = pas de demurrage (comportement historique).
    ///
    /// La valeur effective d'un UTXO à la dépense est
    /// `amount - amount × bps × jours / 10_000` (plancher 0). La conservation
    /// devient `sum(outputs) <= sum(effective_inputs)` pour cet asset — la
    /// décote est brûlée implicitement (réduction de la supply circulante).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demurrage_bps_per_day: Option<u32>,
    /// Mint collatéralisé (protocole 2.3 v2) : adresse de réserve sur le MÊME
    /// ledger. Quand définie, tout mint de cet asset exige que
    /// `(circulating + minted) × collateral_ratio_bps / 10_000` soit couvert
    /// par la somme des UTXOs de `collateral_asset_id` détenus à cette
    /// adresse ET encore time-lockés (`locked_until > now`, protocole 2.1).
    ///
    /// Invariant CONTINU re-vérifié à chaque mint : l'émission totale ne peut
    /// jamais dépasser la réserve actuellement verrouillée — pas de référence
    /// d'UTXO dans le payload, donc pas de double-comptage possible d'une
    /// même réserve entre plusieurs mints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collateral_address: Option<String>,
    /// Asset du collatéral (`None` = natif du ledger). Lu seulement si
    /// `collateral_address` est défini.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collateral_asset_id: Option<String>,
    /// Ratio de couverture en basis points (10_000 = 1:1 numérique entre
    /// montant minté et collatéral verrouillé). OBLIGATOIRE quand
    /// `collateral_address` est défini (validé au registry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collateral_ratio_bps: Option<u32>,
}

/// Métadonnées **publiques** d'une classe semi-fongible (SFT, façon ERC-1155 —
/// `pms-spec-semi-fungibles.md`).
///
/// Une classe est un asset **fongible à l'intérieur de la classe** (quantité par
/// détenteur, divisible selon `decimals`) et **distincte entre classes**. Son
/// `asset_id` est `"{collection_id}:{class_id}"` — le `:` garantit qu'il ne peut
/// jamais entrer en collision avec un token (dont l'`asset_id` ne contient PAS `:`) ni avec
/// le PMS natif (`asset_id = None`). Les soldes vivent dans le moteur UTXO, donc
/// la classe hérite gratuitement du time-lock, du demurrage et des spend-conditions.
///
/// Contrairement aux [`crate::NftMetadata`] (par-instance, chiffrées), les
/// métadonnées de classe sont **un catalogue partagé par N détenteurs** → en clair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SftClass {
    /// Identifiant complet de la classe = `"{collection_id}:{class_id}"`. C'est
    /// l'`asset_id` porté par les UTXO et la clé du registre `sft_classes`.
    pub asset_id: String,
    /// Collection (regroupe plusieurs classes), `[a-z0-9-]{1,32}`.
    pub collection_id: String,
    /// Classe au sein de la collection, `[a-z0-9-]{1,32}`.
    pub class_id: String,
    /// Nom affichable (ex: "Épée de fer"). Non vide, ≤ 128.
    pub name: String,
    /// URI vers l'asset (image, fichier). Optionnel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Attributs de jeu libres (JSON sérialisé). Optionnel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<String>,
    /// Décimales : `0` = items entiers (1 épée), `>0` = divisible. ≤ 18.
    pub decimals: u8,
    /// Supply maximum de la classe (None = illimité). Decimal string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_supply: Option<String>,
    /// Demurrage opt-in (protocole 2.5) : décote en basis points par JOUR PLEIN
    /// écoulé depuis la création de l'UTXO. `None`/`0` = pas de demurrage. Identique
    /// au champ homonyme de [`TokenMetadata`] — les soldes SFT étant des UTXO, la
    /// décote s'applique par le MÊME mécanisme (≤ 10000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demurrage_bps_per_day: Option<u32>,
    /// Adresse/pubkey du créateur (immuable).
    pub creator: String,
    /// Clé publique autorisée à mint cette classe.
    pub mint_authority: String,
}

impl SftClass {
    /// Vue `TokenMetadata` d'une classe SFT, pour réutiliser **telle quelle** la
    /// validation de mint contraint des assets custom (`mint_authority`,
    /// `max_supply`, granularité `decimals` — plan 2.3/2.4). Une classe SFT est,
    /// pour le moteur UTXO, un asset fongible : ses contraintes de mint sont les
    /// mêmes qu'un token. Le demurrage est repris tel quel ; pas de collatéral en v1.
    pub fn to_token_metadata(&self) -> TokenMetadata {
        TokenMetadata {
            asset_id: self.asset_id.clone(),
            symbol: self.class_id.clone(),
            name: self.name.clone(),
            decimals: self.decimals,
            max_supply: self.max_supply.clone(),
            creator: self.creator.clone(),
            mint_authority: self.mint_authority.clone(),
            demurrage_bps_per_day: self.demurrage_bps_per_day,
            collateral_address: None,
            collateral_asset_id: None,
            collateral_ratio_bps: None,
        }
    }
}
