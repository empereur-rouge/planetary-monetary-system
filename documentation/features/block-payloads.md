---
tags: [feature, reference]
created: 2025-05-01
updated: 2026-03-16
version: v0.5.6
---

# Block Payloads & Encryption

## Resume

Le systeme de blocs du DAG-PMS repose sur une architecture a deux niveaux : le **Block** (conteneur) et le **Payload** (contenu metier). Chaque bloc encapsule son payload dans un `PayloadEnvelope` qui supporte deux modes :

- **`Plain`** : payload en clair, utilise en developpement et pour les operations internes du coordinator.
- **`Encrypted`** : payload chiffre via X25519 + AES-256-GCM, utilise en production pour la confidentialite des transactions.

Le modele garantit que le **BlockId** (hash SHA-256) peut etre calcule de maniere deterministe *sans* dechiffrer le contenu, grace a un systeme de `commitment` (hash du plaintext) et de metadonnees publiques (`EnvelopeHeader`).

Le crate `pms-types-payload` definit 18 variantes de `PlainPayload`, couvrant toutes les operations du moteur bancaire : transactions UTXO, minting, NFTs, bridge cross-ledger, compliance (freeze/seize/reverse), contrats declaratifs, distribution de recompenses, et transfert d'ownership de ledger.

---

## Structure d'un Block

Le `Block` est la structure fondamentale du DAG. Il est defini dans `crates/pms-types-block/src/block.rs`.

```rust
pub struct Block {
    pub id: BlockId,                      // hex(sha256(header)), identifiant unique
    pub parents: Vec<String>,             // 1..N parents (DAG, pas blockchain lineaire)
    pub payload: Option<PayloadEnvelope>, // contenu metier (Plain ou Encrypted)
    pub nonce: u64,                       // PoW leger anti-spam

    // Authentification (optionnel pour blocs normaux, requis pour Milestones)
    pub signer_pk: Option<String>,        // cle publique du signataire (hex)
    pub signature: Option<String>,        // signature Ed25519 (hex)

    // Metadonnees hors-consensus
    pub metadata: Option<BlockMetadata>,  // description, tags, extra JSON
}
```

### BlockMetadata

Champ optionnel qui n'est **pas** inclus dans le calcul du `BlockId`. Cela permet d'ajouter des informations lisibles sans affecter le consensus.

```rust
pub struct BlockMetadata {
    pub description: Option<String>,         // description humaine
    pub tags: Vec<String>,                   // tags de categorisation
    pub extra: Option<serde_json::Value>,    // donnees JSON extensibles
    pub signer_x25519_hex: Option<String>,   // cle publique X25519 du signataire (pour fees)
}
```

### Calcul du BlockId

Le `BlockId` est calcule par `compute_block_id()` dans `crates/pms-utils/src/block_id.rs` :

```
BlockId = hex(SHA-256(json({ parents, nonce, envelope_header })))
```

L'`EnvelopeHeader` contient uniquement des metadonnees publiques :

| Champ | Description |
|---|---|
| `payload_type` | Type lisible : `"None"`, `"Genesis"`, `"Mint"`, `"Transaction"`, `"Milestone"`, `"Nft"`, `"ConfigUpdate"`, `"Reward"`, `"EncryptedReward"`, `"TokenCreate"`, `"BridgeLock"`, `"BridgeMint"`, `"Freeze"`, `"Unfreeze"`, `"Seize"`, `"Reverse"`, `"ContractRegister"`, `"ContractUpdate"`, `"LedgerOwnershipTransfer"`, `"Encrypted"` |
| `commitment` | `hex(sha256(plaintext))` -- lie le contenu sans le reveler |
| `len_hint` | Taille approximative (bytes du plaintext ou taille du ciphertext base64) |
| `key_version` | `0` si Plain/None, sinon version de cle pour le chiffrement |

**Propriete critique** : un bloc chiffre et son equivalent en clair produisent le **meme `commitment`**, mais des `BlockId` differents (car `payload_type` et `key_version` different).

---

## Types de Payload

L'enum `PlainPayload` (defini dans `crates/pms-types-payload/src/payload.rs`) contient 18 variantes couvrant toutes les operations du moteur.

### Enveloppe

```rust
pub enum PayloadEnvelope {
    Plain(PlainPayload),         // mode developpement / interne
    Encrypted(EncryptedPayload), // mode production prive
}
```

### Table des variantes PlainPayload

| # | Variante | Champs | Description | Emetteur |
|---|---|---|---|---|
| 1 | `Genesis` | *(aucun)* | Bloc initial du DAG. Un seul par ledger, parents vides, nonce 0. | Systeme |
| 2 | `Mint` | `outputs: Vec<TxOutput>` | Creation de nouveaux tokens (supply initiale ou emission). Chaque output specifie une adresse, un montant, et un asset_id optionnel. | Coordinator |
| 3 | `TxUtxo` | `Transaction` (inputs, outputs, fee, unlocks) | Transaction UTXO standard : consomme des inputs, produit des outputs, paie des frais. Les unlocks contiennent les signatures des emetteurs. Voir [[utxo-system]]. | Utilisateur |
| 4 | `Milestone` | `approved: Vec<String>`, `distribute_node_rewards: bool` | Confirme un ensemble de blocs (par leurs IDs). Si `distribute_node_rewards` est `true`, declenche la distribution du pool de fees aux noeuds proportionnellement a leurs blocs. Voir [[fee-distribution]]. | Coordinator |
| 5 | `Nft` | `NftAction` (enum : Mint, Transfer, Use, Burn, BatchBurn) | Operations sur les NFTs. `Mint` cree un token avec metadonnees, `Transfer` change le proprietaire (avec re-chiffrement optionnel des metadonnees via X25519), `Use` enregistre une utilisation sans destruction, `Burn`/`BatchBurn` detruisent definitivement. Voir [[nft-system]]. | Utilisateur / Coordinator |
| 6 | `ConfigUpdate` | `ConfigUpdate` (enum : SetFeeRate, SetBaseFee, SetCoordinatorFee, SetTreasuryFee, SetMinPow, SetMaxMint, SetMintEnabled, SetFeeTiers, ClearFeeTiers, SetFeeDistribution, SetMintFee, SetTokenCreationFee, SetNftMintFee, SetNftFeeExemptTypes, SetBurnRate, SetContractDeploymentFee, SetStorageFeePerKb, SetDynamicFee, BatchUpdate) | Mise a jour de la configuration du reseau en temps reel. Enregistre dans le DAG pour audit. Voir [[config-system]]. | Coordinator |
| 7 | `Reward` | `fee_outputs: Vec<TxOutput>`, `reward_outputs: Vec<TxOutput>`, `burned: String`, `tx_block_id: String` | Distribution de recompenses en clair (mode dev uniquement). Distribue les fees (treasury, creator, parents) et les block rewards. Le champ `burned` indique le montant deflationniste brule. Reference le bloc de transaction via `tx_block_id`. Voir [[fee-distribution]] et [[economics]]. | Coordinator (auto) |
| 8 | `EncryptedReward` | `encrypted_outputs: Vec<EncryptedRewardOutput>`, `burned: String`, `tx_block_id: String` | Distribution de recompenses chiffrees (mode production). Chaque output est un `EncryptedRewardOutput` chiffre individuellement pour son destinataire + le coordinator. Le montant brule reste public. Voir [[wallet-encryption]]. | Coordinator (auto) |
| 9 | `TokenCreate` | `TokenMetadata` (asset_id, symbol, name, decimals, max_supply, creator, mint_authority) | Enregistrement d'un nouveau token fungible. Definit l'identifiant, le symbole, les decimales, la supply max optionnelle, et l'autorite de minting. Voir [[token-system]]. | Coordinator |
| 10 | `BridgeLock` | `inputs: Vec<TxInput>`, `amount: String`, `asset_id: Option<String>`, `dest_ledger_id: String`, `dest_address: String` | Verrouille des UTXOs sur le ledger source pour un transfert cross-ledger. Les fonds sont detruits sur le ledger source. Voir [[bridge]]. | Coordinator |
| 11 | `BridgeMint` | `outputs: Vec<TxOutput>`, `lock_block_id: String`, `source_ledger_id: String` | Cree des UTXOs sur le ledger destination en referencant un `BridgeLock` source (preuve). Voir [[bridge]]. | Coordinator |
| 12 | `Freeze` | `address: String`, `reason: String` | Gele un compte : bloque toutes les transactions entrantes et sortantes. Reversible via `Unfreeze`. Voir [[compliance]]. | Coordinator |
| 13 | `Unfreeze` | `address: String`, `reason: String`, `freeze_block_id: String` | Degele un compte precedemment gele. Reference le bloc de freeze original. Voir [[compliance]]. | Coordinator |
| 14 | `Seize` | `from_address: String`, `inputs: Vec<TxInput>`, `outputs: Vec<TxOutput>`, `reason: String` | Saisit des UTXOs et les transfere au treasury. Utilise pour les obligations legales (ordonnances judiciaires). Voir [[compliance]]. | Coordinator |
| 15 | `Reverse` | `original_block_id: String`, `inputs: Vec<TxInput>`, `outputs: Vec<TxOutput>`, `reason: String` | Inverse une transaction si ses outputs n'ont pas ete depenses (UTXOs non consommes). Voir [[compliance]]. | Coordinator |
| 16 | `ContractRegister` | `Contract` (contract_id, name, scope, trigger, actions, enabled, version) | Enregistrement d'un contrat declaratif. Le contrat est stocke dans RocksDB et evalue par le `ContractEngine` lors des evenements de trigger. Voir [[smart-contracts]]. | Coordinator |
| 17 | `ContractUpdate` | `contract_id: String`, `enabled: bool`, `reason: String` | Activation ou desactivation d'un contrat existant. Voir [[smart-contracts]]. | Coordinator |
| 18 | `LedgerOwnershipTransfer` | `ledger_id: String`, `encrypted_transfer: EncryptedPayload` | Transfert d'ownership d'un ledger custom. Le `ledger_id` reste en clair pour le routage/validation. Les données sensibles (`OwnershipTransferData`: `new_owner_pubkey`, `reason`) sont chiffrées via X25519+AES-256-GCM pour le coordinator + l'owner actuel + le nouvel owner. Suit le pattern "embedded encryption" comme `EncryptedReward`. Voir [[multi-ledger]]. | Coordinator |

### Types auxiliaires references

| Type | Crate | Description |
|---|---|---|
| `Transaction` | `pms-types-transaction` | `{ inputs: Vec<TxInput>, outputs: Vec<TxOutput>, fee: String, unlocks: Vec<Unlock> }` |
| `TxInput` | `pms-types-transaction` | `{ out: OutputId }` ou `OutputId = { txid: TxId, index: u32 }` |
| `TxOutput` | `pms-types-transaction` | `{ address: String, amount: String, asset_id: Option<String> }` |
| `Unlock` | `pms-types-transaction` | `{ pubkey_hex: String, signature_b64: String }` |
| `NftAction` | `pms-types-nft` | Enum : `Mint { token_id, creator, metadata }`, `Transfer { token_id, from, to, new_owner_x25519_pubkey?, encrypted_metadata? }`, `Use { token_id, user, action_type, action_data? }`, `Burn { token_id, burner }`, `BatchBurn { token_ids, burner }` |
| `NftMetadata` | `pms-types-nft` | `{ name?, description?, uri?, nft_type?, extra? }` |
| `TokenMetadata` | `pms-types-payload` | `{ asset_id, symbol, name, decimals: u8, max_supply?, creator, mint_authority }` |
| `Contract` | `pms-types-contract` | `{ contract_id, name, scope: ContractScope, trigger: ContractTrigger, actions: Vec<ContractAction>, enabled, version }` |
| `ConfigUpdate` | `pms-config` | Enum avec 19 variantes de mise a jour de configuration (voir section ConfigUpdate ci-dessus) |
| `EncryptedRewardOutput` | `pms-types-payload` | `{ encrypted: EncryptedPayload }` -- un `TxOutput` chiffre individuellement |
| `OwnershipTransferData` | `pms-types-payload` | `{ new_owner_pubkey: Option<String>, reason: String }` -- donnees de transfert d'ownership (chiffrees dans le bloc) |

---

## Modele d'Encryption

Le systeme de chiffrement utilise un schema hybride **X25519 + AES-256-GCM** avec enveloppement de cle (key wrapping). Il est implemente dans `crates/pms-types-payload/src/encrypted_payload.rs`.

### Schema : `x25519+aes256gcm`

Constante : `SCHEME_AES256GCM = "x25519+aes256gcm"`
Version de cle courante : `KEY_VERSION_CURRENT = 1`

### Architecture cryptographique

```
                    +-----------------+
                    | PlainPayload    |  (JSON serialise)
                    +--------+--------+
                             |
                    serde_json::to_vec()
                             |
                             v
                    +--------+--------+
                    | plaintext bytes |
                    +--------+--------+
                             |
                    +--------+--------+
                    |    DEK (32B)    |  Data Encryption Key (random)
                    |   nonce (12B)   |  AES-GCM nonce (random)
                    +--------+--------+
                             |
              AES-256-GCM encrypt(DEK, nonce, plaintext, AAD)
                             |
                    +--------+--------+
                    | ciphertext_b64  |  base64(ciphertext + GCM tag)
                    | commitment      |  hex(SHA-256(plaintext))
                    +--------+--------+
                             |
         Pour chaque destinataire (pk X25519 hex) :
                             |
              +-----+--------+--------+-----+
              |              |               |
              v              v               v
     +--------+--+  +--------+--+   +--------+--+
     | Recipient1|  | Recipient2|   | RecipientN|
     +--------+--+  +--------+--+   +--------+--+
              |              |               |
     X25519 ECDH(eph_sk, recip_pk) -> shared_secret
     HKDF-SHA256(shared, salt="pms-dek-wrap", info="kek-v1") -> KEK
     HKDF-SHA256(shared, salt="pms-dek-wrap", info="kid-v1") -> kid (16B, opaque)
     AES-256-GCM wrap(KEK, DEK, AAD=kid) -> wrapped_key
              |              |               |
              v              v               v
     +--------+--------+--------+-----------+
     |        Vec<KeyWrap>                   |
     |  { kid, ephem_pub, wrapped_key_b64,  |
     |    kw_nonce_b64 }                     |
     +---------------------------------------+
```

### Structures de donnees

```rust
pub struct EncryptedPayload {
    pub scheme: String,           // "x25519+aes256gcm"
    pub key_version: u32,         // rotation de cle (actuellement 1)
    pub aad: AAD,                 // metadonnee publique authentifiee
    pub commitment: String,       // hex(sha256(plaintext)) -- liaison au contenu
    pub ciphertext_b64: String,   // base64(AES-256-GCM ciphertext)
    pub recipients: Vec<KeyWrap>, // DEK enveloppee par destinataire
    pub nonce_b64: String,        // base64(nonce 12 bytes pour AES-GCM)
}

pub struct AAD {
    pub len_hint: u32,  // taille du plaintext (ou padding) pour heuristiques
}

pub struct KeyWrap {
    pub kid: String,             // 16 bytes opaques (hex), derives du shared secret
    pub ephem_pub: String,       // cle publique ephemere X25519 (hex)
    pub wrapped_key_b64: String, // base64(AES-256-GCM(KEK, DEK))
    pub kw_nonce_b64: String,    // base64(nonce GCM du wrap)
}
```

### Processus de chiffrement (`encrypt_for`)

1. **Generation de la DEK** : 32 bytes aleatoires (Data Encryption Key).
2. **Generation du nonce** : 12 bytes aleatoires pour AES-GCM.
3. **Chiffrement du payload** : `AES-256-GCM(DEK, nonce, plaintext, AAD={len_hint})`.
4. **Calcul du commitment** : `hex(SHA-256(plaintext))` -- permet de verifier l'integrite apres dechiffrement.
5. **Generation de la cle ephemere** : une paire X25519 unique par message (`eph_sk`, `eph_pk`).
6. **Pour chaque destinataire** :
   - ECDH : `shared = X25519(eph_sk, recipient_pk)`
   - HKDF-SHA256 : `KEK = HKDF(shared, salt="pms-dek-wrap", info="kek-v1")` (32 bytes)
   - HKDF-SHA256 : `kid = HKDF(shared, salt="pms-dek-wrap", info="kid-v1")` (16 bytes)
   - Wrap : `AES-256-GCM(KEK, random_nonce, DEK, AAD=kid)`
7. **Hygiene** : la DEK et la cle ephemere privee sont zeroisees (`zeroize`) apres usage.

### Processus de dechiffrement (`decrypt_with`)

1. **Pour chaque `KeyWrap`** :
   - ECDH : `shared = X25519(recipient_sk, wrap.ephem_pub)`
   - Derive `KEK` et `kid` via HKDF (meme parametres).
   - Si `kid` correspond a `wrap.kid` : tente de dechiffrer la DEK.
2. **Dechiffrement du corps** : `AES-256-GCM(DEK, nonce, ciphertext, AAD)`.
3. **Verification du commitment** : `SHA-256(plaintext_dechiffre) == commitment`.

### Methodes de commodite

| Methode | Signature | Description |
|---|---|---|
| `encrypt_for()` | `(plaintext: &[u8], recipients_pks_hex: &[String], len_hint: u32) -> Result<Self>` | Chiffre des bytes bruts pour N destinataires |
| `encrypt_for_plain()` | `(payload: &PlainPayload, recipients_pks_hex: &[String]) -> Result<Self>` | Serialise un `PlainPayload` en JSON puis chiffre |
| `decrypt_with()` | `(&self, recipient_sk_hex: &str) -> Result<Vec<u8>>` | Dechiffre et retourne les bytes bruts |
| `decrypt_plain_with()` | `(&self, recipient_sk_hex: &str) -> Result<PlainPayload>` | Dechiffre et deserialise en `PlainPayload` |
| `decrypt_as_payload()` | `(&self, recipient_sk_hex: &str) -> Result<PlainPayload>` | Alias de `decrypt_plain_with` |

### Proprietes de securite

| Propriete | Mecanisme |
|---|---|
| **Confidentialite** | AES-256-GCM (chiffrement authentifie) |
| **Integrite** | GCM tag + commitment SHA-256 |
| **Forward secrecy** | Cle ephemere X25519 unique par message |
| **Multi-destinataire** | Chaque destinataire a son propre `KeyWrap` (la DEK est la meme, l'enveloppe differe) |
| **Anonymat des destinataires** | `kid` opaque (derive du shared secret), pas de cle publique du destinataire dans le `KeyWrap` |
| **Hygiene memoire** | `zeroize` sur DEK et cle ephemere privee apres usage |
| **AAD (Authenticated Associated Data)** | `len_hint` authentifie mais non chiffre -- empeche la substitution de ciphertext |

### Relation PlainPayload / EncryptedPayload

```
PlainPayload  ──encrypt_for_plain()──>  EncryptedPayload
                                              │
PayloadEnvelope::Plain(pp)            PayloadEnvelope::Encrypted(ep)
                                              │
EncryptedPayload  ──decrypt_plain_with()──>  PlainPayload
```

- En mode **developpement** : les blocs utilisent `PayloadEnvelope::Plain(PlainPayload::...)` directement.
- En mode **production** : le coordinator chiffre le payload avec `encrypt_for_plain()` avant de creer le bloc, et le stocke dans `PayloadEnvelope::Encrypted(EncryptedPayload { ... })`.
- Le **BlockId** est calcule de maniere identique dans les deux cas : l'`EnvelopeHeader` utilise le `commitment` (hash du plaintext) pour lier le contenu sans le reveler.

### Cas special : EncryptedReward

La variante `PlainPayload::EncryptedReward` est un hybride : le payload lui-meme est en clair dans l'enveloppe (`PayloadEnvelope::Plain`), mais chaque output de recompense est chiffre individuellement via `EncryptedRewardOutput`. Cela permet :

- Le champ `burned` (montant brule) reste **public** pour la transparence deflationniste.
- Le champ `tx_block_id` reste **public** pour la tracabilite.
- Chaque `encrypted_outputs[i].encrypted` est un `EncryptedPayload` contenant un `TxOutput` chiffre pour son destinataire et le coordinator.

### Cas special : LedgerOwnershipTransfer

La variante `PlainPayload::LedgerOwnershipTransfer` suit le meme pattern "embedded encryption" :

- Le champ `ledger_id` reste **public** pour le routage et la validation (le validateur doit savoir quel ledger est concerne).
- Le champ `encrypted_transfer` est un `EncryptedPayload` contenant un `OwnershipTransferData` chiffre pour :
  - Le **coordinator** (toujours, pour l'audit et la gouvernance).
  - L'**owner actuel** du ledger (si `owner_x25519_pubkey` est connu dans `LedgerDef`).
  - Le **nouvel owner** (si `new_owner_x25519_pubkey` est fourni dans la requete).

L'application de l'etat (mise a jour RocksDB + RAM) est effectuee **apres** la persistance du bloc DAG dans l'endpoint API, car le `CoreAdapter` n'a pas acces aux wallets pour le dechiffrement.

---

## Crates et Fichiers

| Crate | Fichier | Contenu |
|---|---|---|
| `pms-types-payload` | `src/payload.rs` | `PayloadEnvelope`, `PlainPayload` (18 variantes), `EncryptedRewardOutput`, `TokenMetadata`, `OwnershipTransferData` |
| `pms-types-payload` | `src/encrypted_payload.rs` | `EncryptedPayload`, `AAD`, `KeyWrap`, logique de chiffrement/dechiffrement X25519+AES-256-GCM |
| `pms-types-payload` | `src/lib.rs` | Re-exports publics |
| `pms-types-payload` | `tests/general.rs` | Tests de roundtrip serde et chiffrement/dechiffrement |
| `pms-types-payload` | `Cargo.toml` | Dependances crypto : `aes-gcm 0.10`, `x25519-dalek 2.0.1`, `hkdf 0.12`, `sha2 0.10`, `zeroize 1.8` |
| `pms-types-block` | `src/block.rs` | `Block`, `BlockMetadata`, constructeurs `new()` et `genesis()` |
| `pms-types-block` | `src/lib.rs` | Re-exports : `Block`, `BlockId`, `BlockMetadata` |
| `pms-utils` | `src/block_id.rs` | `compute_block_id()`, `compute_block_id_sorted()`, `EnvelopeHeader` |
| `pms-types-transaction` | `src/transaction.rs` | `Transaction`, `TxInput`, `TxOutput`, `OutputId`, `Unlock` |
| `pms-types-nft` | `src/action.rs` | `NftAction` (Mint, Transfer, Use, Burn, BatchBurn) |
| `pms-types-nft` | `src/nft.rs` | `Nft`, `NftMetadata` |
| `pms-types-contract` | `src/lib.rs` | `Contract`, `ContractScope`, `ContractTrigger`, `ContractAction`, `MintFormula` |
| `pms-config` | `src/runtime.rs` | `ConfigUpdate` (19 variantes de configuration) |

---

## Types Cles

```rust
// --- Enveloppe ---
pub type BlockId = String;  // hex(sha256(...))

pub enum PayloadEnvelope {
    Plain(PlainPayload),
    Encrypted(EncryptedPayload),
}

// --- Payload metier (18 variantes) ---
pub enum PlainPayload {
    Genesis,
    Mint { outputs: Vec<TxOutput> },
    TxUtxo(Transaction),
    Milestone { approved: Vec<String>, distribute_node_rewards: bool },
    Nft(NftAction),
    ConfigUpdate(ConfigUpdate),
    Reward { fee_outputs: Vec<TxOutput>, reward_outputs: Vec<TxOutput>, burned: String, tx_block_id: String },
    EncryptedReward { encrypted_outputs: Vec<EncryptedRewardOutput>, burned: String, tx_block_id: String },
    TokenCreate(TokenMetadata),
    BridgeLock { inputs: Vec<TxInput>, amount: String, asset_id: Option<String>, dest_ledger_id: String, dest_address: String },
    BridgeMint { outputs: Vec<TxOutput>, lock_block_id: String, source_ledger_id: String },
    Freeze { address: String, reason: String },
    Unfreeze { address: String, reason: String, freeze_block_id: String },
    Seize { from_address: String, inputs: Vec<TxInput>, outputs: Vec<TxOutput>, reason: String },
    Reverse { original_block_id: String, inputs: Vec<TxInput>, outputs: Vec<TxOutput>, reason: String },
    ContractRegister(Contract),
    ContractUpdate { contract_id: String, enabled: bool, reason: String },
    LedgerOwnershipTransfer { ledger_id: String, encrypted_transfer: EncryptedPayload },
}

// --- Chiffrement ---
pub struct EncryptedPayload {
    pub scheme: String,            // "x25519+aes256gcm"
    pub key_version: u32,
    pub aad: AAD,
    pub commitment: String,        // hex(sha256(plaintext))
    pub ciphertext_b64: String,
    pub recipients: Vec<KeyWrap>,
    pub nonce_b64: String,
}
```

---

## Interactions

- [[utxo-system]] : `PlainPayload::TxUtxo` encapsule une `Transaction` avec le modele UTXO (inputs/outputs/unlocks).
- [[wallet-encryption]] : les cles X25519 des wallets sont utilisees comme destinataires pour le chiffrement des `EncryptedPayload` et des `EncryptedRewardOutput`.
- [[nft-system]] : `PlainPayload::Nft(NftAction)` supporte 5 operations NFT, avec re-chiffrement des metadonnees lors des transferts via la cle X25519 du nouveau proprietaire.
- [[smart-contracts]] : `PlainPayload::ContractRegister` et `ContractUpdate` enregistrent et gerent les contrats declaratifs evalue par le `ContractEngine`.
- [[fee-distribution]] : `PlainPayload::Reward` et `EncryptedReward` distribuent les fees et block rewards apres chaque transaction, references via `tx_block_id`.
- [[economics]] : le champ `burned` dans les Reward/EncryptedReward represente le mecanisme deflationniste (fee burn).
- [[bridge]] : `BridgeLock` + `BridgeMint` implementent le transfert atomique cross-ledger avec preuve cryptographique.
- [[compliance]] : `Freeze`, `Unfreeze`, `Seize`, `Reverse` fournissent les outils de conformite reglementaire du coordinator.
- [[config-system]] : `PlainPayload::ConfigUpdate` permet le hot-swap de configuration via le DAG (audit trail immutable).
- [[token-system]] : `PlainPayload::TokenCreate` enregistre de nouveaux tokens fungibles avec leurs metadonnees et autorite de minting.
- [[storage-rocksdb]] : les blocs et leurs payloads sont persistes dans RocksDB avec serialisation JSON.
- [[server-engine]] : le serveur API cree les blocs, valide les payloads, et orchestre le chiffrement/dechiffrement via les cles du coordinator.
