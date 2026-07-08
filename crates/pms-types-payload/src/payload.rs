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
    /// Règlement atomique d'une vente marketplace (protocole 2.7) avec **royalty
    /// de revente enforced au consensus**.
    ///
    /// Déclare une vente — l'item (`asset_sold`, `quantity`) passe du `seller` à
    /// l'`buyer`, le paiement (`price` en `price_asset`) de l'`buyer` au `seller`
    /// — et embarque la transaction UTXO `tx` **co-signée** par les deux parties
    /// (chaque input déverrouillé par son propriétaire, cf. `TxUtxo`). Le
    /// validateur RÉ-DÉRIVE la royalty depuis le registre de `asset_sold`
    /// ([`crate::TokenMetadata::effective_royalty`]) et EXIGE que `tx.outputs`
    /// respecte la forme (item→acheteur, `price×bps/10000`→bénéficiaire dans
    /// `price_asset`, reste→vendeur) — sinon le bloc est **rejeté**. Item ET
    /// paiement bougent dans le même bloc : atomicité tout-ou-rien.
    ///
    /// **Owner-signé** (autorité comme `TxUtxo` : pas coordinator-only ; le bloc
    /// reste forgé/signé par le Coordinator en single-writer). **Payload PLAIN**
    /// (la vente est publique-by-design : prix, parties, royalty auditables).
    ///
    /// Fonctionne pour **tout token / toute classe SFT / tout ledger**, la
    /// royalty étant versée dans l'**asset de paiement** (pas forcément PMS).
    MarketSettle {
        /// Transaction UTXO co-signée portant les mouvements atomiques (inputs
        /// vendeur+acheteur, outputs item/royalty/net/change, `fee` gas brûlé).
        /// Validée par la MÊME `validate_plain_txutxo` que `TxUtxo` (signatures,
        /// ownership, conservation par-asset, compliance) AVANT le gate royalty.
        tx: Transaction,
        /// Asset vendu (token `asset_id` OU classe SFT `"collection:class"`).
        /// Porte la politique royalty. Jamais le PMS natif (on ne « vend » pas
        /// du PMS comme item).
        asset_sold: String,
        /// Quantité de l'item transférée à l'acheteur (décimal string > 0).
        quantity: String,
        /// Asset de paiement (`None` = PMS natif, `Some(x)` = token/SFT). DOIT
        /// différer de `asset_sold`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        price_asset: Option<String>,
        /// Prix total payé par l'acheteur au vendeur, AVANT split royalty (> 0).
        price: String,
        /// Adresse du vendeur (propriétaire actuel de l'item). Ré-vérifiée contre
        /// les propriétaires d'inputs par le validateur (binding de rôle).
        seller: String,
        /// Adresse de l'acheteur (reçoit l'item, paie le prix). Ré-vérifiée idem.
        buyer: String,
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
    /// Met à jour la **politique royalty de revente** d'un asset déjà enregistré
    /// (token OU classe SFT) — protocole 2.7. **Autorisé par la SIGNATURE du
    /// bénéficiaire ACTUEL** (co-signature), PAS par le coordinateur/admin.
    ///
    /// La royalty étant résolue depuis le registre **au moment du settlement**
    /// (pas figée dans l'item au mint), écraser ici `royalty_bps` /
    /// `royalty_beneficiary` fait payer le NOUVEAU bénéficiaire par toutes les
    /// ventes futures. N'affecte AUCUNE vente passée. Les deux champs sont les
    /// nouvelles valeurs ABSOLUES à écrire (mêmes sémantiques que sur
    /// [`TokenMetadata`]/[`SftClass`] : `royalty_bps = None` ⇒ plus de royalty,
    /// `royalty_beneficiary = None` ⇒ défaut = `creator`).
    ///
    /// # Autorité (consensus)
    /// Le validateur EXIGE que `auth_signature_b64` soit une signature valide de
    /// `auth_pubkey_hex` sur [`royalty_update_signing_message`], ET que
    /// `auth_pubkey_hex` dérive l'adresse du **bénéficiaire courant** de l'asset
    /// (ou du `creator` si aucun bénéficiaire explicite). Le bloc reste forgé par
    /// le Coordinator (single-writer) mais le Coordinator NE PEUT PAS rediriger la
    /// royalty sans cette signature. Anti-replay : l'autorisateur devant être le
    /// bénéficiaire *courant*, une signature rejouée après un changement échoue.
    RoyaltyUpdate {
        /// Asset ciblé : `asset_id` d'un token OU `"collection:class"` d'une classe SFT.
        asset_id: String,
        /// Nouveau taux en bps (`None` = plus de royalty). Validé `≤ 10_000`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        royalty_bps: Option<u32>,
        /// Nouveau bénéficiaire Bech32 (`None` = défaut = créateur). Non-vide si présent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        royalty_beneficiary: Option<String>,
        /// Clé publique secp256k1 (hex sec1) de l'AUTORISATEUR = bénéficiaire courant.
        auth_pubkey_hex: String,
        /// Signature ECDSA (base64 DER) de l'autorisateur sur
        /// [`royalty_update_signing_message`]`(network_id, asset_id, royalty_bps,
        /// royalty_beneficiary)`.
        auth_signature_b64: String,
    },
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
            // MarketSettle: the wrapped tx moves item + payment; all its outputs
            // are created UTXOs (item→buyer, royalty→beneficiary, net→seller, change).
            PlainPayload::MarketSettle { tx, .. } => Some(tx.outputs.clone()),
            // No UTXO creation: Genesis, Milestone, ConfigUpdate, EncryptedReward,
            // TokenCreate, BridgeLock, Freeze, Unfreeze, ContractRegister,
            // ContractUpdate, LedgerOwnershipTransfer, CoordinatorKeyRotate, Nft.
            _ => None,
        }
    }

    /// Returns the inner UTXO [`Transaction`] for the payloads that wrap one —
    /// `TxUtxo`, `TokenBurn`, `MarketSettle` — else `None`. Single accessor so
    /// call sites that need the wrapped `inputs`/`outputs`/`fee` (tx lookup, fee
    /// extraction) don't hand-enumerate these variants and can't silently omit
    /// one (mirrors [`Self::outputs`]).
    pub fn tx(&self) -> Option<&Transaction> {
        match self {
            PlainPayload::TxUtxo(tx)
            | PlainPayload::TokenBurn { tx, .. }
            | PlainPayload::MarketSettle { tx, .. } => Some(tx),
            _ => None,
        }
    }
}

/// Validates the resale-royalty policy fields shared by [`TokenMetadata`] and
/// [`SftClass`] (protocole 2.7): `royalty_bps ≤ 10_000` (a money-rule cap), and
/// an explicit `royalty_beneficiary` must be non-empty (omit it to default to
/// the creator). **Single source of truth** — both the token registry
/// (`register_token`) and the SFT-class persist path call this, so the cap can
/// never drift between the two registration paths.
/// Message canonique qu'un bénéficiaire de royalty signe pour AUTORISER un
/// changement de politique (protocole 2.7). Bound au `network_id` (anti-replay
/// cross-chain) + domaine dédié. Renvoie le SHA-256 hex du JSON canonique
/// `{domain, network_id, asset_id, royalty_bps, royalty_beneficiary}` — **source
/// UNIQUE** partagée par le signeur (endpoint/SDK) ET le validateur consensus.
///
/// Anti-replay : la signature commit à l'asset, au `network_id`, à la NOUVELLE
/// politique exacte, ET à la **version courante** de la royalty (`current_version`,
/// compteur monotone). Une signature capturée sur-DAG ne peut pas être rejouée :
/// après tout changement, `royalty_version` avance, donc la version signée ne
/// correspond plus à la version courante et la signature est invalide. Combiné au
/// check « autorisateur == bénéficiaire courant », c'est une autorisation
/// **à usage unique**, liée à l'état exact qu'elle remplace.
pub fn royalty_update_signing_message(
    network_id: &str,
    asset_id: &str,
    royalty_bps: Option<u32>,
    royalty_beneficiary: Option<&str>,
    current_version: u64,
) -> String {
    use sha2::{Digest, Sha256};
    #[derive(Serialize)]
    struct Canon<'a> {
        domain: &'a str,
        network_id: &'a str,
        asset_id: &'a str,
        royalty_bps: Option<u32>,
        royalty_beneficiary: Option<&'a str>,
        current_version: u64,
    }
    let canon = Canon {
        domain: "pms-royalty-update-v1",
        network_id,
        asset_id,
        royalty_bps,
        royalty_beneficiary,
        current_version,
    };
    // Sérialisation d'une struct à champs `&str`/`u32`/`u64` : infaillible et
    // déterministe (ordre de déclaration). `.expect` plutôt qu'un fail-open vers
    // un hash constant (audit : ne jamais dégrader silencieusement un message
    // de sécurité).
    let bytes = serde_json::to_vec(&canon).expect("canonical royalty message is infallible");
    hex::encode(Sha256::digest(bytes))
}

pub fn validate_royalty_fields(
    royalty_bps: Option<u32>,
    royalty_beneficiary: Option<&str>,
) -> Result<(), String> {
    if let Some(bps) = royalty_bps {
        if bps > 10_000 {
            return Err(format!("royalty_bps must be <= 10000, got {bps}"));
        }
    }
    if let Some(b) = royalty_beneficiary {
        if b.trim().is_empty() {
            return Err("royalty_beneficiary cannot be empty (omit it to default to creator)".into());
        }
    }
    Ok(())
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
    /// Royalty de revente (marketplace, protocole 2.7) : part en **basis points**
    /// du PRIX d'une vente [`PlainPayload::MarketSettle`] reversée au
    /// `royalty_beneficiary`. `None`/`0` = pas de royalty. Invariant : `≤ 10_000`
    /// (100 %). Lue au consensus quand cet asset est l'`asset_sold` d'un settlement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub royalty_bps: Option<u32>,
    /// Bénéficiaire de la royalty (Bech32). `None` ⇒ défaut = `creator`. Reversé
    /// dans l'asset de PAIEMENT (pas forcément PMS). Modifiable via `RoyaltyUpdate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub royalty_beneficiary: Option<String>,
    /// Compteur monotone **anti-replay** des changements de royalty (protocole
    /// 2.7). Incrémenté à chaque `RoyaltyUpdate` appliqué ; la signature
    /// d'autorisation commit à la version COURANTE via
    /// [`royalty_update_signing_message`] → une signature capturée sur-DAG ne peut
    /// jamais être rejouée (la version aura avancé). `0` = jamais modifiée.
    #[serde(default)]
    pub royalty_version: u64,
}

impl TokenMetadata {
    /// Résout la politique royalty **effective** de cet asset pour un settlement
    /// marketplace : `Some((bps, beneficiary))` si `royalty_bps > 0`, sinon
    /// `None`. Le bénéficiaire est `royalty_beneficiary` s'il est non-vide, à
    /// défaut le `creator` de l'asset. Source UNIQUE partagée par le builder
    /// serveur ET le validateur consensus (aucune divergence possible).
    pub fn effective_royalty(&self) -> Option<(u32, String)> {
        match self.royalty_bps {
            Some(bps) if bps > 0 => {
                let beneficiary = self
                    .royalty_beneficiary
                    .as_ref()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| self.creator.clone());
                Some((bps, beneficiary))
            }
            _ => None,
        }
    }
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
    /// Royalty de revente (marketplace, protocole 2.7) : part en **basis points**
    /// du PRIX d'une vente [`PlainPayload::MarketSettle`] de cette classe reversée
    /// au `royalty_beneficiary`. `None`/`0` = pas de royalty. `≤ 10_000`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub royalty_bps: Option<u32>,
    /// Bénéficiaire de la royalty (Bech32). `None` ⇒ défaut = `creator`. Reversé
    /// dans l'asset de PAIEMENT du settlement (pas forcément PMS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub royalty_beneficiary: Option<String>,
    /// Compteur monotone anti-replay des changements de royalty (cf. champ homonyme
    /// de [`TokenMetadata`]). `0` = jamais modifiée.
    #[serde(default)]
    pub royalty_version: u64,
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
            royalty_bps: self.royalty_bps,
            royalty_beneficiary: self.royalty_beneficiary.clone(),
            royalty_version: self.royalty_version,
        }
    }
}

#[cfg(test)]
mod royalty_tests {
    use super::*;

    fn base_meta() -> TokenMetadata {
        TokenMetadata {
            asset_id: "tkn".into(),
            symbol: "TKN".into(),
            name: "Token".into(),
            decimals: 8,
            max_supply: None,
            creator: "pms1creator".into(),
            mint_authority: "pms1creator".into(),
            demurrage_bps_per_day: None,
            collateral_address: None,
            collateral_asset_id: None,
            collateral_ratio_bps: None,
            royalty_bps: None,
            royalty_beneficiary: None,
            royalty_version: 0,
        }
    }

    #[test]
    fn royalty_signing_message_golden() {
        // Vecteurs GOLDEN pour la parité cross-implémentation (SDK TypeScript).
        // Le SDK DOIT produire le MÊME JSON compact (ordre = déclaration, sans
        // espaces, `None`→`null`, `Some(x)`→`x`, u64 en nombre) puis SHA-256 hex.
        // JSON attendu (vecteur 1) :
        //   {"domain":"pms-royalty-update-v1","network_id":"pms-dev-v1",
        //    "asset_id":"studio:ticket","royalty_bps":2000,
        //    "royalty_beneficiary":"8e1abc","current_version":0}
        let v1 = royalty_update_signing_message("pms-dev-v1", "studio:ticket", Some(2000), Some("8e1abc"), 0);
        // JSON attendu (vecteur 2, cas None) :
        //   {"domain":"pms-royalty-update-v1","network_id":"pms-dev-v1",
        //    "asset_id":"col:cls","royalty_bps":null,
        //    "royalty_beneficiary":null,"current_version":3}
        let v2 = royalty_update_signing_message("pms-dev-v1", "col:cls", None, None, 3);
        println!("GOLDEN v1 = {v1}");
        println!("GOLDEN v2 = {v2}");
        // Golden hardcodés (indépendants de la formule) — toute divergence côté SDK
        // OU tout changement du format de message casse ce test (bien voulu).
        assert_eq!(v1, "5ef3ba01105c96e4e1ab07a702ee4b2ccb2e21aa15f4243956261bd4ebbaeb4e", "PIN v1");
        assert_eq!(v2, "4ae5830ef7f4ce6d8a966fc3fd19053cc3b40094e3e5930c813621b9dec5880f", "PIN v2");
    }

    #[test]
    fn effective_royalty_none_when_absent_or_zero() {
        let mut m = base_meta();
        assert_eq!(m.effective_royalty(), None, "no royalty_bps → None");
        m.royalty_bps = Some(0);
        assert_eq!(m.effective_royalty(), None, "royalty_bps=0 → None");
        println!("effective_royalty absent/zero → None: OK");
    }

    #[test]
    fn effective_royalty_defaults_beneficiary_to_creator() {
        let mut m = base_meta();
        m.royalty_bps = Some(2000);
        let got = m.effective_royalty();
        println!("royalty 2000 bps, no beneficiary → {got:?}");
        assert_eq!(got, Some((2000, "pms1creator".to_string())));
    }

    #[test]
    fn effective_royalty_uses_explicit_beneficiary() {
        let mut m = base_meta();
        m.royalty_bps = Some(500);
        m.royalty_beneficiary = Some("pms1studio".into());
        let got = m.effective_royalty();
        println!("royalty 500 bps → {got:?}");
        assert_eq!(got, Some((500, "pms1studio".to_string())));
    }

    #[test]
    fn effective_royalty_blank_beneficiary_falls_back_to_creator() {
        let mut m = base_meta();
        m.royalty_bps = Some(1000);
        m.royalty_beneficiary = Some("   ".into());
        let got = m.effective_royalty();
        println!("royalty 1000 bps, blank beneficiary → {got:?}");
        assert_eq!(got, Some((1000, "pms1creator".to_string())), "blank → creator");
    }

    #[test]
    fn sft_class_carries_royalty_into_token_metadata() {
        let class = SftClass {
            asset_id: "col:cls".into(),
            collection_id: "col".into(),
            class_id: "cls".into(),
            name: "Item".into(),
            uri: None,
            attributes: None,
            decimals: 0,
            max_supply: Some("10".into()),
            demurrage_bps_per_day: None,
            creator: "pms1artist".into(),
            mint_authority: "pms1coord".into(),
            royalty_bps: Some(1500),
            royalty_beneficiary: None,
            royalty_version: 0,
        };
        let tm = class.to_token_metadata();
        println!("SFT class royalty → token_metadata: {:?}", tm.effective_royalty());
        // Beneficiary defaults to the class CREATOR (the artist), not the mint_authority.
        assert_eq!(tm.effective_royalty(), Some((1500, "pms1artist".to_string())));
    }
}
