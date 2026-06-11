---
tags: [feature, security]
created: 2025-12-15
updated: 2026-06-11
version: v0.9.0
---

# Validation & Consensus Rules

## Resume

Le systeme de validation du PMS Engine constitue le rempart de securite fondamental du reseau. Il garantit que chaque bloc insere dans le DAG respecte un ensemble strict de regles structurelles, cryptographiques, economiques et de conformite.

> **v0.9.0 — Remédiation audit sécurité (2026-06-11).** Le hot path de
> production (`do_persist_block_internal`) exécute désormais lui-même TOUTE
> l'autorisation, inconditionnellement :
> - **C-1/C-2** : `validate_transaction_full()` vérifie les signatures des
>   `unlocks` ET le binding ownership (la pubkey de `unlock[i]` doit dériver
>   l'adresse propriétaire de l'UTXO dépensé par `input[i]`) — dépenser
>   l'UTXO d'autrui est rejeté (`OwnershipMismatch`).
> - **C-2 ext.** : l'autorité coordinator-only par type de payload est
>   centralisée dans `validations/authority.rs`, appelée par le hot path ET
>   le legacy `validate_block` (avant : hot path sans aucun check signataire
>   pour ConfigUpdate/Freeze/Reward/etc.).
> - **H-3** : la validation UTXO ne dépend plus de `skip_utxo_checks`.
> - **H-4** : single-writer **fail-closed** — active set vide ⇒ rejet de
>   tous les blocs (sauf mode Dev pur).
> - **M-6** : le block id est recalculé depuis le contenu canonique à
>   l'ingestion ; tout mismatch est rejeté.
> - **M-7** : conservation stricte PAR ASSET + sanity du champ `fee`
>   (non-négatif, ≤ `max_fee_per_tx`) — règle canonique unique.

L'architecture repose sur un **pipeline de validation en deux phases** :

1. **Phase Wire-Level** (`net_adapter/persist.rs`) : validation du bloc reseau brut (`WireBlock`) -- identite reseau, signature cryptographique du bloc, Single Writer enforcement (fail-closed), intégrité du block id (M-6), autorité par payload (authority.rs), taille payload, parents uniques, NFT/Mint/Config/Compliance, et **autorisation complète des TxUtxo** (`validate_transaction_full`).

2. **Phase DAG-Level** (`check.rs` / `validate_block()`, chemin legacy dag.rs/tests) : validation semantique contre l'etat du DAG -- anti-cycle, nombre de parents, regles par type de payload. **La verification des signatures de TX UTXO inclut le `network_id` de la chaine** -- une TX signee pour un autre reseau (testnet vs mainnet) est rejetee comme `InvalidSignature` (cross-chain replay protection, v0.8.0).

Le consensus repose sur un **Single Writer Protocol** : en mode production (Mainnet/Testnet), seul le Coordinator (cle publique hardcodee dans `pms-consensus`) peut creer des blocs. La finalite est determinee par les **Milestones** (checkpoints signes par le Coordinator) et/ou par la **k-depth finality** (nombre de descendants confirmant un bloc).

La cryptographie utilise **ECDSA secp256k1** (meme courbe que Bitcoin) via la librairie `k256`, avec verification parallele des signatures de transaction via `rayon`.

## Pipeline de Validation

### Vue d'ensemble (ordre d'execution)

Chaque bloc passe par deux pipelines sequentiels. Le premier (`persist_block` dans `net_adapter.rs`) traite le `WireBlock` brut recu du reseau. Le second (`validate_block` dans `check.rs`) traite le `Block` reconstruit contre l'etat du DAG.

```
WireBlock recu du reseau
         |
         v
+========================================+
|  PHASE 1 : WIRE-LEVEL (net_adapter/)   |
+========================================+
|                                        |
|  1.a) network_id / protocol_version   |
|       --> Rejet si mismatch            |
|                                        |
|  1.b) Cle publique obligatoire        |
|       --> signer_pk_hex non vide       |
|                                        |
|  1.c) Signature obligatoire           |
|       --> signature_hex non vide       |
|                                        |
|  1.d) Verification cryptographique    |
|       --> verify_block_signature(wb)   |
|       --> ECDSA secp256k1 / SHA-256    |
|                                        |
|  1.e) Single Writer Enforcement       |
|       --> single_writer_gate()         |
|       FAIL-CLOSED: active set vide ->  |
|       rejet (sauf Dev pur) [H-4]       |
|                                        |
|  2.b) Taille payload brut (anti-spam) |
|       --> payload_json.len() <= max    |
|                                        |
|  2.c) Deserialisation PayloadEnvelope |
|       --> JSON -> Plain/Encrypted      |
|                                        |
|  1.v) Integrite du block id [M-6]     |
|       --> id == compute_block_id()     |
|       (hash canonique du contenu)      |
|                                        |
|  1.w) Autorite par payload [C-2 ext]  |
|       --> validate_payload_authority() |
|       (authority.rs, rotation-aware)   |
|                                        |
|  1.x) Politique de Mint               |
|       --> validate_mint_policy()       |
|       --> validate_mint_security()     |
|                                        |
|  1.y) Validation NFT                  |
|       --> validate_nft_action()        |
|       --> Ownership, existence, signer |
|                                        |
|  1.z) ConfigUpdate                    |
|       --> apply_config_update()        |
|                                        |
|  1.compliance) Freeze/Unfreeze/Seize  |
|       --> Registre compliance          |
|                                        |
|  2.d) Parents uniques + self-parent   |
|       --> HashSet deduplication        |
|                                        |
|  2.e) Minimum parents (multi-writer)  |
|       --> min_parents_after_boot       |
|                                        |
|  2.f) Single Writer: 1 parent exact   |
|       --> enforce_single_parent()      |
|                                        |
+========================================+
         |
         v  (Block reconstruit)
+========================================+
|  PHASE 2 : DAG-LEVEL (check.rs)       |
+========================================+
|                                        |
|  4.a) Parents existent (RAM + RocksDB)|
|       --> contains_block() || store    |
|                                        |
|  4.new) AUTORISATION TXUTXO COMPLETE  |
|       --> validate_transaction_full()  |
|       INCONDITIONNEL [C-1/C-2/H-3/M-7]:|
|       - inputs.len == unlocks.len      |
|       - fee sanity (>=0, <= max)       |
|       - verify_tx_signatures()         |
|       - ownership binding par input    |
|         (unlock_matches_address)       |
|       - existence + anti double-spend  |
|       - conservation par asset         |
|                                        |
|  4.compliance) Freeze check           |
|       --> is_frozen() sur sender/recip |
|       (reutilise les outputs fetches)  |
|                                        |
+========================================+
         |
         v
+========================================+
|  validate_block() (check.rs)           |
+========================================+
|                                        |
|  1) payload_size_limit()              |
|     --> Anti-spam (taille serialisee)  |
|                                        |
|  2) no_cycle() + parent_count()       |
|     --> Structure DAG                  |
|                                        |
|  3) Regles par PlainPayload:          |
|     Genesis -> genesis_rules()         |
|     Mint    -> amounts_positive()      |
|     TxUtxo  -> verify_tx_signatures()  |
|              -> validate_fee_recip()   |
|              -> tx_amounts_valid()     |
|              -> utxo_no_double_spend() |
|              -> utxo_sufficient_funds()|
|     Milestone -> coordinator_pk check  |
|     Nft      -> signer_pk required     |
|     ConfigUpdate -> coordinator check  |
|     Reward   -> coordinator check      |
|     EncryptedReward -> coord check     |
|     TokenCreate -> coordinator check   |
|     BridgeLock -> coord + inputs+dest  |
|     BridgeMint -> coord + outputs+ref  |
|     Freeze   -> coord + address        |
|     Unfreeze -> coord + addr + ref     |
|     Seize    -> coord + inputs/outputs |
|     Reverse  -> coord + block_id + io  |
|     ContractRegister -> coord + fields |
|     ContractUpdate -> coord + id       |
|     Encrypted -> structure only (MVP)  |
|                                        |
+========================================+
         |
         v
  Bloc accepte --> Persistence RocksDB
                --> Insertion RAM DAG
                --> UTXO delta applique
                --> Finalite mise a jour
```

### Strategie de court-circuit

Le pipeline est ordonne du **moins couteux au plus couteux** en termes de calcul. Chaque etape retourne un `Result<(), ValidationError>` et court-circuite immediatement en cas d'echec, evitant ainsi de gaspiller du CPU sur un bloc invalide :

1. Taille payload (comparaison de longueur -- O(1))
2. Structure parents (HashSet -- O(n) avec n petit)
3. Regles payload-specifiques (selon type)
4. Verification de signatures (ECDSA -- couteux)
5. Validation UTXO (recherche en RAM/store -- I/O)

## ValidatePolicy (regles configurables par ledger)

La struct `ValidatePolicy` centralise tous les parametres de validation, configurables par ledger via le fichier TOML ou la `RuntimeConfig` hot-swappable.

### Champs et valeurs par defaut

| Champ | Type | Defaut | Description |
|-------|------|--------|-------------|
| `max_payload_bytes` | `usize` | 65 536 (64 KB) | Taille maximale du payload serialise (anti-spam) |
| `min_parents_after_boot` | `usize` | 2 | Nombre minimum de parents apres bootstrap du DAG |
| `max_parents` | `usize` | 8 | Nombre maximum de parents par bloc |
| `require_unique_parents` | `bool` | `true` | Interdit les parents dupliques |
| `forbid_self_parent` | `bool` | `true` | Interdit l'auto-parentage (cycle trivial) |
| `max_inputs` | `usize` | 64 | Maximum d'inputs par transaction UTXO |
| `max_outputs` | `usize` | 64 | Maximum d'outputs par transaction UTXO |
| `max_tx_bytes` | `usize` | 65 536 (64 KB) | Taille maximale d'une transaction serialisee |
| `min_pow_leading_zero_bits` | `u8` | 0 | Bits de difficulte PoW (deprecie en Single Writer) |
| `max_fee_per_tx` | `Decimal` | 1000 | Fee maximum autorise par transaction |
| `enforce_parent_existence` | `bool` | `true` | Verifie que les parents existent dans le store |
| `enforce_fee_recipient` | `bool` | `false` | Impose que le dernier output soit un fee recipient autorise |
| `allowed_fee_addresses` | `Vec<String>` | `[]` | Liste blanche d'adresses pour les fees |
| `skip_utxo_checks` | `bool` | `false` | Si true, les checks UTXO sont faits en async (Phase 4) |
| `platform_address` | `Option<String>` | `None` | Adresse plateforme pour le split de fees |
| `platform_fee_ratio` | `Decimal` | 0 | Ratio de fees vers la plateforme (0-1) |
| `coordinator_public_key` | `Option<String>` | `None` | Cle publique hex du Coordinator |
| `enforce_single_writer` | `bool` | `true` | Active le mode Single Writer (chaine lineaire) |

### Sources de configuration

La `ValidatePolicy` peut etre construite de trois facons :

1. **`ValidatePolicy::default()`** : valeurs par defaut pour les tests.

2. **`ValidatePolicy::from_settings(v: &ValidationSettings)`** : depuis la config TOML statique. Utilisee au demarrage.

3. **`ValidatePolicy::try_from_global_config()`** : version complete qui charge la config globale, resout la cle Coordinator selon le `NetworkMode` (Mainnet/Testnet/Dev), et verifie la signature de la `platform_address`.

4. **`update_from_runtime_config(config: &RuntimeConfig)`** : mise a jour dynamique (Hot-Swap) depuis un bloc `ConfigUpdate`. Modifie `platform_fee_ratio` et `min_pow_leading_zero_bits` a chaud.

### Securite de la Platform Address

En mode Mainnet/Testnet, la `platform_address` (adresse recevant une part des fees) doit etre **signee cryptographiquement** par la cle Coordinator. La verification se fait via `verify_config_signature()` :

```
address (bytes) --> Sign(coordinator_privkey, address) --> signature_hex
                          |
verify_config_signature(address, signature_hex, coordinator_pk_hex)
  --> VerifyingKey::from_sec1_bytes(pk)
  --> Signature::from_der(sig) || Signature::from_slice(sig)
  --> vk.verify(address.as_bytes(), &sig) == Ok
```

Erreurs possibles :
- `InvalidPlatformSignature` : la signature ne correspond pas a la cle Coordinator.
- `MissingPlatformSignature` : pas de signature fournie en mode production.

### Protection Dev/Prod

La construction de la policy inclut une protection contre l'utilisation de cles de production en mode Dev :

```rust
// En mode Dev, on ne doit JAMAIS utiliser les cles Mainnet/Testnet
if is_mainnet_key || is_testnet_key {
    return Err(ValidationError::ProdKeyInDevMode { network: "MAINNET" });
}
```

## Regles par Type de Payload

### Genesis

- Le bloc genesis doit avoir **zero parent**.
- Il doit etre le **premier bloc** du DAG (`n_existing == 0`).
- Validation : `genesis_rules()` + `parent_count()`.

### Mint

**Validation en 3 couches :**

1. **Montants positifs** (`amounts_positive_outputs()`) : chaque output doit avoir un montant `> 0` en notation decimale valide.

2. **Politique de mint** (`validate_mint_policy()` dans `policy.rs`) :
   - Verifie que le signataire est dans `admin.signer_pubkeys` (ou variable env `PMS_TEST_ADMIN_PUBKEY` en mode Dev).
   - En Testnet/Mainnet, la liste `signer_pubkeys` est obligatoire.
   - Verifie le montant total : `sum(outputs) <= MAX_REWARD_PER_BLOCK (1 000 000)`.
   - Les montants negatifs sont rejetes.

3. **Securite Coordinator** (`validate_mint_security()` dans `mint.rs`) :
   - Si `coordinator_public_key` est configure, le signataire du bloc **doit** correspondre (comparaison case-insensitive).
   - En mode Dev sans cle configuree : mint autorise avec warning.

### TxUtxo (Transaction UTXO)

**Production (hot path, v0.9.0)** : `validate_transaction_full()` dans
`transactions.rs` — exécutée INCONDITIONNELLEMENT par
`do_persist_block_internal` pour tout payload `Plain(TxUtxo)`. Ordre :

1. **Appariement** : `inputs.len() == unlocks.len()` — `unlock[i]` autorise `input[i]`.
2. **Fee sanity (M-7)** : `tx.fee` décimal non-négatif et `<= max_fee_per_tx`.
   Le fee est déclaratif — sa valeur doit être un output explicite (sinon la
   conservation échoue). Aucun surplus n'est brûlé implicitement.
3. **Signatures (C-2)** : `verify_tx_signatures()` sur le message canonique
   `{network_id, inputs, outputs, fee}` ; déduplication des unlocks
   identiques avant l'ECDSA (wallets mono-clé).
4. **Ownership (C-1)** : pour chaque input, `unlock_matches_address(pubkey,
   utxo.address)` (`ownership.rs`) — supporte adresse pubkey hex brute (SDK)
   et bech32m (`SHA256(pubkey)[..20] || x25519`). Échec ⇒ `OwnershipMismatch`.
5. **Existence + double-spend + conservation stricte par asset**.
6. La fonction retourne les `TxOutput` des inputs — réutilisés par le freeze
   check compliance sans second lookup.

**Chemin chiffré** : `wallet_send_tx` (`POST /v1/wallet/tx/send`) applique
les MÊMES checks (appariement, signatures, ownership, conservation par asset)
sur le plaintext AVANT chiffrement — le hot path ne voit que le ciphertext.

**Legacy (`validate_block`, dag.rs/tests)** — pipeline historique, dans l'ordre :

1. **Verification des signatures** (`verify_tx_signatures(tx, network_id)` dans `signature.rs`) :
   - `inputs.len() == unlocks.len()` (correspondance 1:1).
   - Message canonique calcule via `tx.signing_message(network_id)` -- le `network_id` provient de `policy.network_id` (lui-meme issu de `Settings.network.network_id`).
   - **Cross-chain replay protection (v0.8.0)** : le `network_id` est inclus dans le JSON canonique signe `{network_id, inputs, outputs, fee}`. Une TX signee pour un autre reseau (ex: testnet) est rejetee comme `InvalidSignature("signature mismatch input N")` puisque le hash recompute differe. La protection est intrinseque au signing -- aucun champ `network_id` n'est ajoute au wire format, ce qui empeche un attaquant de declarer son network_id de signature.
   - Chaque unlock : decode pubkey hex -> SEC1 VerifyingKey, decode signature base64 -> DER Signature, verifie ECDSA.
   - **Optimisation hybride** : sequentiel pour < 4 inputs (overhead rayon), parallele via `rayon::par_iter` pour >= 4 inputs.
   - Tests : `crates/pms-core/tests/tx_validation.rs::reject_tx_signed_for_different_network`.

2. **Fee recipient** (`validate_fee_recipient_output()` dans `fees.rs`) :
   - Si `platform_address` est configuree avec un `platform_fee_ratio > 0` : verifie que les outputs contiennent une part suffisante vers la plateforme.
   - Si `enforce_fee_recipient` est active : le dernier output doit etre vers une adresse dans `allowed_fee_addresses`.

3. **Montants et quotas** (`tx_amounts_valid()` dans `amount.rs`) :
   - `inputs.len() <= max_inputs` (defaut 64).
   - `outputs.len() <= max_outputs` (defaut 64).
   - Taille serialisee `<= max_tx_bytes` (defaut 64 KB).
   - Tous les outputs > 0.
   - Fee >= 0 et <= `max_fee_per_tx`.

4. **Anti-double-spend** (`utxo_no_double_spend()` dans `transactions.rs`) :
   - Verification intra-transaction : pas de doublons dans les inputs (HashSet).
   - Verification globale : chaque outpoint n'est pas dans `dag.spent_outpoints`.

5. **Fonds suffisants** (`utxo_sufficient_funds()` dans `transactions.rs`) :
   - `sum(inputs) >= sum(outputs) + fee`.
   - Les montants des inputs sont resolus en cherchant les blocs parents (Mint ou TxUtxo).

6. **Validation Async (Phase 4)** (`validate_transaction_async()`) :
   - Si `skip_utxo_checks = true`, la validation UTXO est faite via le `ShardedUtxoSet` (lock-free) au lieu du DAG RAM.
   - Verifie la **conservation par asset** : `sum(inputs[asset]) == sum(outputs[asset])` pour chaque `asset_id`.
   - Empeche la creation d'un asset sans input correspondant.

7. **Compliance Freeze Check** :
   - Verifie que ni les adresses sender (owners des inputs) ni les adresses recipient (outputs) ne sont gelees via `is_frozen()`.

### Milestone

- Le signataire **doit** etre le Coordinator (`coordinator_public_key`).
- Si aucune cle Coordinator n'est configuree : rejet avec `Other("Milestones not enabled")`.
- Les Milestones sont des checkpoints qui finalisent les blocs approuves.
- Le champ `distribute_node_rewards: bool` declenche la distribution du fee pool aux mineurs.

### NFT (Nft)

**Validation en deux temps :**

1. **Phase sync** (`check.rs`) : le bloc doit etre signe (`signer_pk` non-None).

2. **Phase async** (`nft.rs` via `validate_nft_action()`) : validation complete contre le `NftStorage` (RocksDB).

| Action | Token existe ? | Signer autorise | Regles supplementaires |
|--------|----------------|-----------------|----------------------|
| **Mint** | Non (sinon `TokenAlreadyExists`) | Coordinator (si configure) + creator == signer | -- |
| **Transfer** | Oui (sinon `TokenNotFound`) | Owner actuel OU Coordinator | `from == owner` actuel |
| **Use** | Oui | Owner OU Coordinator | `user == owner` |
| **Burn** | Oui | Owner OU Coordinator | `burner == owner` |
| **BatchBurn** | Oui (chaque token) | Burner OU Coordinator | Chaque `burner == owner` |

### ConfigUpdate

- Signataire **doit** etre le Coordinator.
- Applique la mise a jour au store via `apply_config_update()`.
- Persiste le nouvel etat `RuntimeConfig` dans RocksDB.

### Reward / EncryptedReward

- Signataire **doit** etre le Coordinator.
- Cree des UTXOs de distribution (fee_outputs + reward_outputs).
- `EncryptedReward` : meme regles, mais outputs chiffres individuellement pour chaque destinataire.

### TokenCreate

- Signataire **doit** etre le Coordinator.
- Enregistre un nouveau token dans le DAG avec ses metadonnees (asset_id, symbol, decimals, max_supply, mint_authority).

### BridgeLock

- Signataire **doit** etre le Coordinator.
- Au moins un input requis (`inputs.is_empty() -> rejet`).
- `dest_ledger_id` et `dest_address` obligatoires (non-vides).
- En mode async : `validate_bridge_lock_async()` verifie que `sum(inputs) >= amount` et que l'`asset_id` correspond.

### BridgeMint

- Signataire **doit** etre le Coordinator.
- Au moins un output requis.
- `lock_block_id` et `source_ledger_id` obligatoires (non-vides).
- Reference un `BridgeLock` sur le ledger source (preuve cross-ledger).

### Compliance (Freeze, Unfreeze, Seize, Reverse)

Toutes les operations de conformite utilisent `require_coordinator_signature()` :

| Payload | Champs valides | Effets |
|---------|---------------|--------|
| **Freeze** | `address` non-vide | Gele le compte via `freeze_address()` |
| **Unfreeze** | `address` + `freeze_block_id` non-vides | Degele via `unfreeze_address()` + log compliance |
| **Seize** | `inputs` + `outputs` non-vides, `from_address` non-vide | UTXO delta (spend + create) + log compliance |
| **Reverse** | `original_block_id` + `inputs` + `outputs` non-vides | UTXO delta inverse + log compliance |

### Smart Contracts (ContractRegister, ContractUpdate)

- Signataire **doit** etre le Coordinator.
- `ContractRegister` : `contract_id`, `name`, et `actions` doivent etre non-vides.
- `ContractUpdate` : `contract_id` doit etre non-vide.

### Encrypted (PayloadEnvelope::Encrypted)

- En MVP : seules les validations structurelles (taille, parents) sont appliquees.
- Le contenu chiffre ne peut pas etre valide sans la cle de dechiffrement.
- A terme : validations "header-only" (quota, taille, destinataires).

## Single Writer Protocol

### Principe

En mode Single Writer (`enforce_single_writer = true`, defaut), le DAG fonctionne comme une **chaine lineaire** : chaque bloc a exactement 1 parent (sauf le genesis qui en a 0). Seul le Coordinator peut creer des blocs.

```
Genesis --> Block A --> Block B --> Block C --> Block D  (chaine)
   0 parent   1 parent   1 parent   1 parent   1 parent

vs. DAG classique (multi-writer, enforce_single_writer = false) :

Genesis --> A --> C --> E
       \-> B --> D /    (DAG avec merges)
```

### Avantages

- **Ordre total deterministe** : pas de conflit, pas d'orphelins.
- **Finalite immediate** : chaque bloc est immediatement confirme.
- **Performance maximale** : pas de resolution de conflits, pas de k-depth BFS.
- **Simplicite** : selection de tip triviale (dernier bloc de la chaine).

### Enforcement (2 niveaux)

1. **Niveau WireBlock** (`net_adapter/persist.rs`, etape 1.e) :
   - Tous les blocs doivent etre signes par `coordinator_public_key`.
   - Si le signataire differe : `PutResult::Rejected("single_writer: only Coordinator can create blocks")`.

2. **Niveau structure** (`net_adapter/persist.rs`, etape 2.f + `parents.rs`) :
   - Non-genesis : exactement 1 parent (`wb.parents.len() != 1` -> rejet).
   - Genesis : 0 parents.
   - Fonction `enforce_single_parent()` dans `parents.rs`.

### Desactivation

Mettre `enforce_single_writer = false` dans `[validation]` pour activer le mode multi-writer (DAG complet avec selection de tips, k-depth finality, resolution de conflits).

## Coordinator Authentication

### Cles Hardcodees

Les cles publiques du Coordinator sont definies comme constantes dans `pms-consensus` :

```rust
// Mainnet
pub const COORDINATOR_PUBLIC_KEY_MAINNET: &str =
    "036ed4d5ad1c927fe972ef9728ac1888d237af57a488b6cbe50228fac442b5ae6b";

// Testnet
pub const COORDINATOR_PUBLIC_KEY_TESTNET: &str =
    "02115e0941c01a05f6d6dfc6aa9204e20d0d1af9d3231c25728034d9278bf7187f";
```

### Resolution de la cle active

La cle Coordinator utilisee pour la validation est resolue dans cet ordre de priorite :

1. **Config override** (`validation.coordinator_public_key` dans le fichier TOML) : si present, utilise cette cle. En Mainnet/Testnet, un warning est emis si elle differe de la cle hardcodee.

2. **Cle hardcodee** (`pms-consensus`) : selon le `NetworkMode` :
   - `Mainnet` -> `COORDINATOR_PUBLIC_KEY_MAINNET`
   - `Testnet` -> `COORDINATOR_PUBLIC_KEY_TESTNET`
   - `Dev` -> `None` (pas de verification Coordinator en Dev sans config explicite)

### Payloads Coordinator-Only

**v0.9.0** : les règles d'autorité par type de payload sont centralisées dans
`validations/authority.rs::validate_payload_authority()`, appelée par les
DEUX chemins — le hot path (`do_persist_block_internal`, étape 1.w, AVANT
tout apply d'état, avec la clé courante rotation-aware) et le legacy
`validate_block()` (étape 3). Elles ne peuvent plus diverger.

Payloads couverts (autorité Coordinator + structure minimale) : `Milestone`,
`ConfigUpdate`, `Reward`, `EncryptedReward`, `TokenCreate`, `BridgeLock`,
`BridgeMint`, `Freeze`, `Unfreeze`, `Seize`, `Reverse`, `ContractRegister`,
`ContractUpdate`, `LedgerOwnershipTransfer`, `CoordinatorKeyRotate`.

Hors périmètre (validation dédiée) : `Mint` → `validate_mint_security()`
(réutilise la même policy rotation-aware), `TxUtxo` →
`validate_transaction_full()`, `Nft` → `validate_nft_action()`, `Genesis`.

En mode Dev pur (aucune clé coordinator configurée), l'enforcement est sauté
avec un warn — en Testnet/Mainnet la clé est toujours présente (constantes
hardcodées), ce chemin n'existe pas en production.

### Verification cryptographique des blocs

La signature de chaque bloc est verifiee par `verify_block_signature()` dans `crypto.rs` :

```
1. pk_bytes = hex::decode(signer_pk_hex)            // Cle publique SEC1
2. vk = VerifyingKey::from_sec1_bytes(pk_bytes)      // Secp256k1
3. sig_bytes = base64_decode(signature_hex)           // Signature en base64
4. sig = Signature::from_der(sig_bytes)               // DER ou raw 64 bytes
   || Signature::from_bytes(sig_bytes)
5. msg = canonical_wireblock_message(wb)              // Message canonique
6. vk.verify(msg.as_bytes(), &sig)                    // ECDSA verify (SHA-256)
```

Garanties :
- **Authenticite** : le bloc a ete signe par le detenteur de la cle privee.
- **Integrite** : aucune donnee n'a ete modifiee apres la signature.
- **Non-repudiation** : le signataire ne peut pas nier avoir signe.

## Finalite

### Milestones

Les blocs `Milestone` sont des checkpoints signes par le Coordinator. Quand un Milestone est insere :
- `finality.last_milestone = Some(block.id)`.
- Le Milestone lui-meme est immediatement marque comme finalise.
- Si `distribute_node_rewards = true`, le fee pool est distribue aux mineurs proportionnellement a leur nombre de blocs.

### K-depth Finality

La finalite par profondeur est configuree via `FinalityState.depth_k` :
- Un bloc est finalise quand il a au moins `k` descendants distincts (BFS).
- L'algorithme est **incremental** : a chaque insertion, seuls les ancetres du nouveau bloc (dans la fenetre `depth_k`) sont verifies, reduisant la complexite de O(N * k) a O(k^2).
- Fonction cle : `ancestors_within_depth()` + `count_descendants()` dans `ConcurrentDag`.

### Application en memoire

Apres validation, les effets sont appliques en RAM via `apply_block_mem()` dans `apply.rs` :
1. Deduplication des parents.
2. Mise a jour des compteurs d'enfants (`bump_children`).
3. Index parent -> enfants (`add_child_edge_mem`).
4. Marquage des outpoints spent (TxUtxo, BridgeLock, Seize, Reverse).
5. Enregistrement du bloc dans le DAG.
6. Mise a jour de la finalite (`update_finality_after_insert`).

## Crates et Fichiers

### pms-core (validation principale)

| Fichier | Role |
|---------|------|
| `crates/pms-core/src/validations/mod.rs` | Module racine des validations |
| `crates/pms-core/src/validations/check.rs` | Point d'entree `validate_block()`, `ValidatePolicy`, `verify_config_signature()` |
| `crates/pms-core/src/validations/authority.rs` | `validate_payload_authority()` — autorité coordinator-only par payload, partagée hot path + legacy (v0.9.0) |
| `crates/pms-core/src/validations/ownership.rs` | `unlock_matches_address()` — binding pubkey ↔ propriétaire UTXO (C-1, v0.9.0) |
| `crates/pms-core/src/validations/policy.rs` | `validate_mint_policy()`, `check_mint_amount()` |
| `crates/pms-core/src/validations/mint.rs` | `validate_mint_security()`, `validate_mint_security_logic()` |
| `crates/pms-core/src/validations/transactions.rs` | `validate_transaction_full()` (hot path v0.9.0), `validate_transaction_async()`, `check_asset_conservation()`, `utxo_no_double_spend()`, `utxo_sufficient_funds()`, `validate_bridge_lock_async()` |
| `crates/pms-core/src/validations/signature.rs` | `verify_tx_signatures()` (dédup des unlocks identiques), `verify_single_signature()` |
| `crates/pms-core/src/validations/fees.rs` | `validate_fee_recipient_output()` |
| `crates/pms-core/src/validations/amount.rs` | `amount_parse_pos_dec()`, `amount_parse_non_neg_dec()`, `amounts_positive_outputs()`, `tx_amounts_valid()`, `amount_is_positive_decimal()` |
| `crates/pms-core/src/validations/nft.rs` | `validate_nft_action()`, `NftValidationError` |
| `crates/pms-core/src/validations/parents.rs` | `parents_exist_in_store()`, `no_cycle()`, `parent_count()`, `enforce_single_parent()` |
| `crates/pms-core/src/validations/traits.rs` | Trait `WriteState` (effets en memoire) |
| `crates/pms-core/src/validations/impls.rs` | Implementation de `WriteState` pour `Dag` |
| `crates/pms-core/src/validations/apply.rs` | `apply_block_mem()` (application des effets en RAM) |

### pms-core (infrastructure)

| Fichier | Role |
|---------|------|
| `crates/pms-core/src/net_adapter/mod.rs` | Re-exports du module net_adapter |
| `crates/pms-core/src/net_adapter/persist.rs` | `persist_block()` -- pipeline wire-level complet |
| `crates/pms-core/src/crypto/crypto.rs` | `verify_block_signature()` -- ECDSA secp256k1 |
| `crates/pms-core/src/finality.rs` | `FinalityState`, `has_k_confirmations_dag()` |
| `crates/pms-core/src/concurrent_dag/mod.rs` | `ConcurrentDag` -- DAG lock-free avec DashMap |

### pms-consensus

| Fichier | Role |
|---------|------|
| `crates/pms-consensus/src/lib.rs` | `COORDINATOR_PUBLIC_KEY_MAINNET`, `COORDINATOR_PUBLIC_KEY_TESTNET` |

### pms-config

| Fichier | Role |
|---------|------|
| `crates/pms-config/src/config.rs` | `ValidationSettings`, `NetworkMode`, `FeesSettings` |
| `crates/pms-config/src/runtime.rs` | `RuntimeConfig`, `ConfigUpdate` (hot-swap) |

### pms-types

| Fichier | Role |
|---------|------|
| `crates/pms-types-payload/src/payload.rs` | `PlainPayload`, `PayloadEnvelope` -- tous les types de payload |

## Fonctions Cles

### validate_block()
```rust
pub fn validate_block(dag: &Dag, b: &Block, policy: &ValidatePolicy) -> Result<(), ValidationError>
```
Point d'entree unique de la validation DAG-level. Orchestre les fonctions specialisees dans un ordre du moins couteux au plus couteux. Court-circuite des la premiere erreur.

### persist_block()
```rust
async fn persist_block(&self, wb: &WireBlock) -> Result<PutResult>
```
Pipeline complet d'insertion d'un bloc recu du reseau. Inclut la validation wire-level, la reconstruction du `Block`, la validation DAG-level, la persistence RocksDB, et la mise a jour de la finalite.

### verify_block_signature()
```rust
pub fn verify_block_signature(wb: &WireBlock) -> Result<()>
```
Verifie la signature ECDSA secp256k1 d'un `WireBlock`. Utilise le message canonique `canonical_wireblock_message(wb)` et supporte les signatures DER et raw 64 bytes.

### verify_tx_signatures()
```rust
pub fn verify_tx_signatures(tx: &Transaction) -> Result<(), ValidationError>
```
Verifie toutes les signatures d'une transaction. Strategie hybride : sequentiel pour < 4 inputs, parallele via rayon pour >= 4 inputs.

### validate_transaction_full() (v0.9.0 — hot path)
```rust
pub async fn validate_transaction_full(utxos: &ShardedUtxoSet, tx: &Transaction, policy: &ValidatePolicy) -> Result<Vec<TxOutput>, ValidationError>
```
Autorisation COMPLÈTE d'une TxUtxo : appariement input↔unlock, fee sanity,
signatures ECDSA (canonical avec network_id), binding ownership par input,
existence + anti double-spend + conservation par asset. Retourne les outputs
des inputs pour réutilisation (freeze check). Tests d'attaque :
`crates/pms-core/tests/spend_authorization.rs`.

### unlock_matches_address() (v0.9.0)
```rust
pub fn unlock_matches_address(pubkey_hex: &str, address: &str) -> bool
```
Binding C-1 : true si la pubkey ECDSA dérive l'adresse (forme brute hex ou
bech32m `SHA256(pubkey)[..20]`).

### validate_payload_authority() (v0.9.0)
```rust
pub fn validate_payload_authority(signer_pk: Option<&str>, payload: Option<&PayloadEnvelope>, policy: &ValidatePolicy) -> Result<(), ValidationError>
```
Autorité coordinator-only + structure minimale pour les 15 payloads sensibles.
Appelée par le hot path (clé rotation-aware) et `validate_block`.

### validate_transaction_async()
```rust
pub async fn validate_transaction_async(utxos: &ShardedUtxoSet, tx: &Transaction) -> Result<(), ValidationError>
```
Validation UTXO lock-free via le `ShardedUtxoSet`. Verifie l'absence de doublons internes, l'existence des inputs, et la conservation par asset. ⚠️ Ne vérifie ni signatures ni ownership — préférer `validate_transaction_full` en production.

### validate_nft_action()
```rust
pub fn validate_nft_action<S: NftStorage>(action: &NftAction, signer_pk_hex: &str, coordinator_pk: Option<&str>, nft_store: &S) -> Result<()>
```
Valide les actions NFT (Mint, Transfer, Use, Burn, BatchBurn) contre le `NftStorage`. Verifie l'ownership, l'existence du token, et les autorisations.

### require_coordinator_signature()
```rust
fn require_coordinator_signature(b: &Block, policy: &ValidatePolicy, action_name: &str) -> Result<(), ValidationError>
```
Helper generique pour les payloads Coordinator-only. Verifie que `b.signer_pk == policy.coordinator_public_key`.

### ValidatePolicy::try_from_global_config()
```rust
pub fn try_from_global_config() -> Result<Self, ValidationError>
```
Construit une `ValidatePolicy` complete depuis la configuration globale, incluant la resolution de la cle Coordinator selon le `NetworkMode` et la verification de la signature de la `platform_address`.

## Interactions

- **[[server-engine]]** : Le serveur Axum utilise `persist_block()` (via le trait `NetDagAdapter`) comme point d'entree pour l'ingestion de blocs. L'`AppState` contient le `CoreAdapter` qui encapsule le `ConcurrentDag`, le store RocksDB, et la `ValidatePolicy`. La `RuntimeConfig` (rechargee depuis RocksDB a chaque bloc) permet le hot-swap des parametres de validation via `update_from_runtime_config()`.

- **[[config-system]]** : La `ValidatePolicy` est construite depuis `ValidationSettings` (config TOML statique) et mise a jour dynamiquement via `RuntimeConfig` (blocs `ConfigUpdate` signes par le Coordinator). Les parametres economiques (fee ratios, PoW bits) sont hot-swappables sans redemarrage.

- **[[utxo-system]]** : La validation UTXO utilise deux mecanismes complementaires :
  1. **RAM DAG** (`dag.spent_outpoints`) : verification rapide anti-double-spend dans `utxo_no_double_spend()`.
  2. **ShardedUtxoSet** : validation async lock-free dans `validate_transaction_async()`, avec conservation par asset.
  Les effets sont appliques en RAM via `apply_block_mem()` (mark_spent_ram) et en RocksDB via le `UtxoDelta` atomique.

- **[[fee-distribution]]** : `validate_fee_recipient_output()` verifie la conformite des outputs de fees (platform split, allowed addresses). La `platform_fee_ratio` est configuree dans la `ValidatePolicy` et mise a jour via `RuntimeConfig`.

- **[[nft-system]]** : `validate_nft_action()` est appele dans `persist_block()` avec acces au `NftStorage` RocksDB. La validation complete (ownership, existence) ne peut se faire que dans cette phase async, pas dans `validate_block()` synchrone.

- **[[compliance]]** : Les payloads de conformite (Freeze, Unfreeze, Seize, Reverse) utilisent `require_coordinator_signature()` et sont appliques dans `persist_block()` via les methodes du trait `ComplianceStorage`.

- **[[multi-ledger]]** : Chaque ledger possede sa propre `ValidatePolicy`, permettant des regles de validation differentes par ledger. Les operations cross-ledger (`BridgeLock`, `BridgeMint`) ont des validations specifiques (inputs non-vides, references croisees).

- **[[smart-contracts]]** : `ContractRegister` et `ContractUpdate` sont valides comme tout payload Coordinator-only. Les champs metier (contract_id, name, actions) sont verifies dans `validate_block()`.
