---
tags: [feature]
created: 2026-01-08
updated: 2026-03-13
version: v0.1.0
---

# NFT System (Mint, Burn, Transfer, Refund)

## Résumé

Le système NFT de PMS permet la création, le transfert, l'utilisation et la destruction de tokens non-fongibles (Non-Fungible Tokens) avec un modèle **Privacy-First** : les métadonnées sont systématiquement chiffrées (X25519 + AES-256-GCM) et ne sont jamais stockées en clair. Seuls le propriétaire actuel et le Coordinateur peuvent déchiffrer les données. Lors d'un transfert, le Coordinateur effectue un re-chiffrement atomique des métadonnées pour le nouveau propriétaire. Le système s'intègre avec le moteur de [[smart-contracts|contrats déclaratifs]] pour permettre des mécanismes de burn-to-refund (remboursement automatique lors de la destruction de NFTs).

## Dates

| | Date |
|---|---|
| Créée | 2026-01-08 |
| Dernière mise à jour | 2026-03-13 |
| Version d'introduction | v0.1.0 |

## Configuration

### Coordinateur (Single Writer Mode)

- **Mint** : Seul le Coordinateur peut minter des NFTs. La clé publique du Coordinateur (`coordinator_public_key` dans la mint policy) est vérifiée lors de la validation. En mode dev (pas de coordinateur configuré), le mint est ouvert.
- **Transfer / Burn / Use** : Le propriétaire signe l'action. En Single Writer Mode, le Coordinateur peut également signer au nom du propriétaire.

### Frais NFT

- `nft_mint_fee` : Frais de mint (configurable par ledger via `EffectiveFees` ou `RuntimeConfig`). Par défaut `0.5 PMS`.
- `nft_fee_exempt_types` : Liste de `nft_type` exempts de frais de mint (ex: `"reward"`).
- `storage_fee_per_kb` : Surcharge calculée sur la taille du payload des métadonnées (en KB).
- Les frais sont distribués via un reward block ou accumulés dans le `FeePool` en fallback.

### [[economics|Gas Pool]] (Ledgers custom)

- Chaque endpoint NFT (mint, burn, burn-simple, burn-batch-simple) vérifie `try_consume_gas()` pour les ledgers non-main, rejetant la requête avec `402 PAYMENT_REQUIRED` si le gas pool est épuisé.

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-types-nft` | `crates/pms-types-nft/src/lib.rs` | Ré-export public : `Nft`, `NftMetadata`, `NftAction` |
| `pms-types-nft` | `crates/pms-types-nft/src/nft.rs` | Structure `Nft` et `NftMetadata` (token_id, owner, creator, metadata) |
| `pms-types-nft` | `crates/pms-types-nft/src/action.rs` | Enum `NftAction` : Mint, Transfer, Use, Burn, BatchBurn |
| `pms-types-nft` | `crates/pms-types-nft/tests/nft_tests.rs` | Tests unitaires des types NFT (création, sérialisation, action_type_str) |
| `pms-types-payload` | `crates/pms-types-payload/src/payload.rs` | `PlainPayload::Nft(NftAction)` -- intégration dans le système de payloads |
| `pms-types-payload` | `crates/pms-types-payload/src/encrypted_payload.rs` | `EncryptedPayload` : chiffrement X25519 + AES-256-GCM des métadonnées |
| `pms-storage` | `crates/pms-storage/src/nft_store.rs` | Trait `NftStorage` + mock `InMemoryNftStore` pour les tests |
| `pms-storage` | `crates/pms-storage/src/rocks_store/nft_storage.rs` | Implémentation RocksDB de `NftStorage` (3 column families) |
| `pms-storage` | `crates/pms-storage/src/rocks_store/store.rs` | Déclaration des CFs NFT dans `CF_NAMES` et dans `new()` |
| `pms-core` | `crates/pms-core/src/validations/nft.rs` | Validation des actions NFT (ownership, existence, autorisation) |
| `pms-core` | `crates/pms-core/src/net_adapter.rs` | Application des actions NFT lors du `persist_block()` dans le DAG |
| `pms-core` | `crates/pms-core/tests/nft_validation.rs` | Tests de validation NFT (mint success, already exists, unauthorized, etc.) |
| `pms-server` | `crates/pms-server/src/api_fn/nft.rs` | Endpoints API : mint, burn, burn-simple, burn-batch-simple, get, list, prepare-transfer |
| `pms-server` | `crates/pms-server/src/api.rs` | Déclaration des routes NFT dans le routeur Axum |
| `pms-server` | `crates/pms-server/src/contract_engine.rs` | `evaluate_nft_burn()` : évaluation des contrats déclaratifs après burn |
| `pms-server` | `crates/pms-server/src/fee_pool.rs` | `add_burn_refund()` : accumulation des remboursements de burn |
| `pms-server` | `crates/pms-server/src/api_fn/tx_helpers.rs` | `load_nft_mint_fee()`, `is_nft_type_fee_exempt()` : calcul des frais NFT |
| `pms-server` | `crates/pms-server/tests/nft_e2e.rs` | Tests E2E : mint + verify ownership via le DAG complet |
| `pms-event` | `crates/pms-event/src/events.rs` | `PmsEvent::Nft { block_id, action }` : événements NFT émis après validation |
| `pms-storage` | `crates/pms-storage/src/helpers.rs` | `ActivityCategory::Nft` : catégorisation des activités NFT pour le [[activity-system|wallet activity feed]] |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `validate_nft_action()` | `crates/pms-core/src/validations/nft.rs` | Valide une action NFT : existence du token, ownership, autorisation du signataire. Supporte le mode Coordinateur. |
| `apply_mint()` | `crates/pms-storage/src/nft_store.rs` | Enregistre un NFT (token_id -> owner) + référence au bloc (token_id -> block_id). Privacy-first : pas de metadata en clair. |
| `apply_transfer()` | `crates/pms-storage/src/nft_store.rs` | Change le propriétaire et met à jour le block_id (re-encryption : le bloc Transfer devient la nouvelle référence). |
| `apply_action()` | `crates/pms-storage/src/nft_store.rs` | Dispatch générique : Transfer simple (sans re-encryption), Use (no-op), Burn (delete), BatchBurn (delete multiple). |
| `set_owner()` | `crates/pms-storage/src/rocks_store/nft_storage.rs` | Dual-write RocksDB : met à jour `nft_ownership` ET `nfts_by_owner` (index inverse). Gère le retrait de l'ancien owner. |
| `delete()` | `crates/pms-storage/src/rocks_store/nft_storage.rs` | Supprime un NFT de `nft_ownership` et nettoie l'index inverse `nfts_by_owner`. |
| `mint_nft()` | `crates/pms-server/src/api_fn/nft.rs` | Endpoint POST : chiffre les métadonnées (X25519 pour owner + coordinateur), forge le bloc, persiste, indexe l'activité, distribue les frais. |
| `burn_nft()` | `crates/pms-server/src/api_fn/nft.rs` | Endpoint POST : accepte un WireBlock pré-signé avec NftAction::Burn ou BatchBurn, persiste, applique, évalue les contrats. |
| `burn_nft_simple()` | `crates/pms-server/src/api_fn/nft.rs` | Endpoint POST simplifié : le serveur construit et signe le bloc à partir de la clé privée fournie (comme send-simple). |
| `burn_nft_batch_simple()` | `crates/pms-server/src/api_fn/nft.rs` | Endpoint POST : burn de multiples NFTs en un seul bloc. Vérifie l'ownership de chaque token avant la destruction. |
| `prepare_nft_transfer()` | `crates/pms-server/src/api_fn/nft.rs` | Endpoint POST : le Coordinateur déchiffre les métadonnées, re-chiffre pour le nouveau propriétaire, retourne l'action Transfer prête à signer. |
| `decrypt_nft_metadata_from_dag()` | `crates/pms-server/src/api_fn/nft.rs` | Déchiffre les métadonnées d'un NFT depuis le bloc DAG. Supporte les blocs Mint (Encrypted) et Transfer (encrypted_metadata imbriquée). |
| `evaluate_contracts_after_burn()` | `crates/pms-server/src/api_fn/nft.rs` | Orchestre l'évaluation des [[smart-contracts|contrats déclaratifs]] après un burn réussi. Pré-fetch metadata AVANT apply_action (qui supprime le block_id). |
| `evaluate_nft_burn()` | `crates/pms-server/src/contract_engine.rs` | Recherche les contrats matchant le type de burn, évalue les formules (FixedRate, AttributeFormula, FixedAmount), retourne les refunds. |
| `get_nft()` | `crates/pms-server/src/api_fn/nft.rs` | Endpoint GET : retourne ownership + block_id (pour déchiffrement client). Ne retourne jamais les métadonnées en clair. |
| `get_nfts_by_owner()` | `crates/pms-server/src/api_fn/nft.rs` | Endpoint GET : liste des token_ids possédés par une adresse. |
| `load_nft_mint_fee()` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Charge le frais de mint NFT depuis RuntimeConfig ou EffectiveFees (priorité : RuntimeConfig > per-ledger config). |
| `is_nft_type_fee_exempt()` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Vérifie si un `nft_type` est exempt de frais de mint (ex: "reward"). |

## Endpoints API

| Méthode | Path | Description |
|---------|------|-------------|
| `GET` | `/v1/nft/{token_id}` | Informations publiques d'un NFT (owner, exists, mint_block_id). Ne retourne pas les métadonnées en clair. |
| `GET` | `/v1/wallet/{address}/nfts` | Liste des token_ids possédés par une adresse (avec count). |
| `POST` | `/v1/nft/mint` | Mint un NFT. Le Coordinateur chiffre les métadonnées pour l'owner + coordinateur. Body: `MintNftRequest`. |
| `POST` | `/v1/nft/burn` | Burn un NFT via WireBlock pré-signé (NftAction::Burn ou BatchBurn). Le client signe le bloc. |
| `POST` | `/v1/nft/burn-simple` | Burn simplifié : le serveur forge et signe le bloc à partir de `private_key_b64` + `token_id`. |
| `POST` | `/v1/nft/burn-batch-simple` | Burn batch simplifié : destruction de multiples NFTs en un seul bloc. Body: `BurnNftBatchSimpleRequest`. |
| `POST` | `/v1/nft/transfer/prepare` | Prépare un transfert avec re-chiffrement. Le Coordinateur déchiffre puis re-chiffre pour le nouveau propriétaire. Retourne l'action Transfer prête à signer. |

### Notes sur les endpoints

- **Mint** (`/v1/nft/mint`) : Le `token_id` doit faire exactement 64 caractères hex. Les métadonnées sont chiffrées avec le schéma `x25519+aes256gcm` pour 2 destinataires (owner + coordinateur).
- **Burn** (`/v1/nft/burn`) : Le bloc doit être signé par le propriétaire (le `burner` dans l'action doit correspondre au `signer_pk_hex` du bloc, ou au Coordinateur en mode Single Writer).
- **Transfer** : Le flux en 2 étapes -- (1) `POST /v1/nft/transfer/prepare` retourne l'action avec métadonnées re-chiffrées, (2) le client signe et soumet via `POST /v1/blocks`.

## RocksDB Column Families

| Column Family | Clé | Valeur | Description |
|---------------|-----|--------|-------------|
| `nft_ownership` | `token_id` (bytes) | `owner_address` (bytes) | Ownership principal : quel wallet possède ce token |
| `nfts_by_owner` | `owner_address` (bytes) | JSON array de `token_id`s | Index inverse : liste des tokens possédés par une adresse. Sérialisé en `["token1", "token2", ...]` |
| `nft_block_ids` | `token_id` (bytes) | `block_id` (bytes) | Référence au bloc DAG contenant les métadonnées chiffrées. Mis à jour lors du Mint et du Transfer avec re-encryption. |

### Notes sur le stockage

- **Privacy-First** : Les métadonnées ne sont JAMAIS stockées en clair dans RocksDB. Seule la référence `block_id` est stockée. Les métadonnées chiffrées résident dans le bloc du DAG.
- **Dual-write** : `set_owner()` effectue un double write atomique (`nft_ownership` + `nfts_by_owner`). Lors d'un transfert, l'ancien propriétaire est retiré de sa liste avant d'ajouter le nouveau.
- **Nettoyage au burn** : `delete()` supprime l'entrée de `nft_ownership` ET nettoie l'index inverse. `delete_block_id()` supprime la référence au bloc.
- Les 3 CFs sont déclarées dans `CF_NAMES` (pour `open_db_multi_prefix`) et dans la liste hardcodée de `new()` dans `store.rs`. Les deux listes doivent être synchronisées.

## NFT Actions

L'enum `NftAction` (défini dans `crates/pms-types-nft/src/action.rs`) comprend 5 variants :

### `Mint`
- **Champs** : `token_id: String`, `creator: String`, `metadata: NftMetadata`
- **Validation** : Le token ne doit pas déjà exister. Le `creator` doit correspondre au `signer_pk_hex`. En production, seul le Coordinateur peut minter.
- **Stockage** : `apply_mint(token_id, creator, block_id)` -- privacy-first, pas de metadata en clair.
- **Payload** : `PayloadEnvelope::Encrypted(EncryptedPayload)` -- les métadonnées sont chiffrées pour owner + coordinateur.

### `Transfer`
- **Champs** : `token_id`, `from`, `to`, `new_owner_x25519_pubkey: Option<String>`, `encrypted_metadata: Option<String>`
- **Validation** : Le token doit exister. `from` doit être le owner actuel. Le signataire doit être le owner ou le Coordinateur.
- **Avec re-encryption** : Si `new_owner_x25519_pubkey` est fourni, `apply_transfer()` met à jour le `block_id` pour pointer vers le bloc Transfer (contenant les métadonnées re-chiffrées).
- **Sans re-encryption** : Transfer simple via `set_owner()` -- le `block_id` reste inchangé (ancien owner garde théoriquement accès aux métadonnées).

### `Use`
- **Champs** : `token_id`, `user`, `action_type: String`, `action_data: Option<String>`
- **Validation** : Le token doit exister. `user` doit être le owner. Le signataire doit être le user ou le Coordinateur.
- **Stockage** : Aucun changement d'ownership. L'action est enregistrée dans le DAG uniquement (traçabilité).
- **Cas d'usage** : Confirmation de clicker, rédemption de ticket, achievements.

### `Burn`
- **Champs** : `token_id`, `burner`
- **Validation** : Le token doit exister. `burner` doit être le owner. Le signataire doit être le burner ou le Coordinateur.
- **Stockage** : Supprime `block_id` puis supprime l'entrée ownership (et l'index inverse).
- **Post-burn** : Évaluation des [[smart-contracts|contrats déclaratifs]] via `evaluate_contracts_after_burn()`.

### `BatchBurn`
- **Champs** : `token_ids: Vec<String>`, `burner`
- **Validation** : Vérifie l'ownership de chaque token individuellement. Le signataire doit être le burner ou le Coordinateur.
- **Stockage** : Supprime chaque token séquentiellement (`delete_block_id` + `delete` pour chacun).
- **Post-burn** : Évaluation des contrats déclaratifs pour l'ensemble du batch.

## Métadonnées NFT

La structure `NftMetadata` (définie dans `crates/pms-types-nft/src/nft.rs`) est extensible :

| Champ | Type | Description |
|-------|------|-------------|
| `name` | `Option<String>` | Nom du NFT (ex: "Ticket Concert 2024") |
| `description` | `Option<String>` | Description textuelle |
| `uri` | `Option<String>` | URI vers l'asset (image, fichier) |
| `nft_type` | `Option<String>` | Type de NFT (ex: "ticket", "collectible", "clicker", "cube") |
| `extra` | `Option<String>` | Données supplémentaires en JSON libre |

Le champ `nft_type` est utilisé par :
- Le système de frais (`nft_fee_exempt_types`) pour exempter certains types de frais de mint.
- Le moteur de [[smart-contracts|contrats]] (`ContractTrigger::OnNftBurn { nft_type }`) pour cibler les contrats sur des types spécifiques.
- Le [[activity-system|système d'activité]] (`ActivityCategory::Nft`) pour catégoriser les opérations NFT dans le wallet activity feed.

## Chiffrement des Métadonnées

### Schéma cryptographique

- **Algorithme** : X25519 (échange de clés) + AES-256-GCM (chiffrement symétrique authentifié)
- **Schema identifier** : `x25519+aes256gcm`
- **Key version** : `1` (rotation de clé supportée)
- **Commitment** : `hex(sha256(plaintext))` -- vérification d'intégrité

### Flux de chiffrement au Mint

1. Les métadonnées sont sérialisées en JSON (`serde_json::to_vec(&metadata)`)
2. `EncryptedPayload::encrypt_for()` génère une clé symétrique éphémère (DEK)
3. Le DEK est enveloppé (key-wrap) pour chaque destinataire via X25519 ECDH
4. Les destinataires sont : `[owner_x25519_pubkey, coordinator_x25519_pubkey]`
5. Le payload chiffré est emballé dans `PayloadEnvelope::Encrypted`

### Flux de re-chiffrement au Transfer

1. Le Coordinateur déchiffre les métadonnées depuis le bloc source (Mint ou Transfer précédent)
2. Il re-chiffre pour `[new_owner_x25519_pubkey, coordinator_x25519_pubkey]`
3. Le payload chiffré est sérialisé en JSON string et inclus dans `NftAction::Transfer { encrypted_metadata }`
4. Le bloc Transfer est soumis ; son `block_id` remplace l'ancien dans `nft_block_ids`

### Déchiffrement côté client

```
GET /v1/wallet/{address}/nfts  ->  liste d'IDs
GET /v1/nft/{id}               ->  { mint_block_id }
GET /v1/blocks/{mint_block_id} ->  bloc brut avec EncryptedPayload
Client: decrypt(payload, x25519_private_key) -> NftMetadata JSON
```

## Interactions

### [[smart-contracts|Smart Contracts]] (burn-to-refund)

Le système NFT s'intègre avec le moteur de contrats déclaratifs (`crates/pms-server/src/contract_engine.rs`) :

- **Trigger** : `ContractTrigger::OnNftBurn { nft_type: Option<String> }` -- déclenché par les burns NFT
- **Formules** : `FixedRate` (taux fixe par token), `AttributeFormula` (basée sur les attributs des métadonnées), `FixedAmount` (montant fixe)
- **Scope** : `ContractScope::Global` ou `ContractScope::Ledger(vec)` pour ciblage par ledger
- **Flux** :
  1. Les métadonnées sont pré-fetchées AVANT `apply_action()` (car le burn supprime le `block_id`)
  2. Après un burn réussi, `evaluate_contracts_after_burn()` recherche les contrats matchant
  3. Les refunds calculés sont accumulés dans le `FeePool` via `add_burn_refund()`
  4. Les refunds sont distribués lors du prochain Milestone ([[fee-distribution|distribution de fees]])

### [[fee-distribution|Fee Distribution]]

- Les frais de mint NFT (`nft_mint_fee` + `storage_fee`) sont distribués via un reward block créé immédiatement après le mint.
- Si la création du reward block échoue, les frais sont accumulés dans le `FeePool` en fallback.
- Les burn refunds (issus des contrats) suivent le mécanisme standard de distribution de fees (treasury/coordinator/parents).

### [[activity-system|Activity System]]

- Chaque opération NFT est indexée dans le système d'activité via `write_addr_activity_entries_with_categories()`
- La catégorie `ActivityCategory::Nft` est assignée automatiquement pour toutes les actions NFT
- Pour les mints chiffrés, l'indexation est faite explicitement dans `mint_nft()` avec l'adresse du propriétaire
- Pour les burns et transfers, la catégorisation est extraite automatiquement depuis le `PlainPayload` via `extract_involved_with_category()`

### Event System

- Chaque action NFT validée émet un `PmsEvent::Nft { block_id, action }` (défini dans `crates/pms-event/src/events.rs`)
- Les types d'événements générés : `nft_minted`, `nft_transferred`, `nft_used`, `nft_burned`, `nft_batch_burned`

### Block Processing (net_adapter)

- Dans `CoreAdapter::persist_block()`, les payloads `PlainPayload::Nft` sont interceptés :
  1. `validate_nft_action()` est appelé avec le signer, le coordinateur, et le store
  2. Si valide, l'action est appliquée au store selon le type :
     - `Mint` -> `apply_mint(token_id, creator, block_id)`
     - `Transfer` avec `new_owner_x25519_pubkey` -> `apply_transfer(token_id, to, block_id)`
     - `Transfer` simple -> `set_owner(token_id, to)`
     - `Burn`, `Use`, `BatchBurn` -> `apply_action(action)`
  3. Si l'application échoue, le bloc est rejeté (`PutResult::Rejected`)

## Tests

| Fichier de test | Description |
|----------------|-------------|
| `crates/pms-types-nft/tests/nft_tests.rs` | Tests unitaires : création, metadata, sérialisation JSON, action_type_str |
| `crates/pms-core/tests/nft_validation.rs` | Tests de validation : mint success, mint already exists, transfer unauthorized, burn unauthorized, coordinator mode |
| `crates/pms-server/tests/nft_e2e.rs` | Tests E2E : mint via DAG complet (Store + DAG + CoreAdapter), vérification ownership |
