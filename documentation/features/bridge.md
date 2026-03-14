---
tags: [feature]
created: 2026-02-13
updated: 2026-03-13
version: v0.1.0
---

# Bridge (Transferts Cross-Ledger)

## Résumé

Le Bridge est le système de transfert de fonds entre ledgers distincts du réseau PMS. Il implémente un mécanisme **lock-and-mint atomique** : les fonds sont verrouillés (détruits) sur le ledger source via un bloc `BridgeLock`, puis recréés sur le ledger destination via un bloc `BridgeMint`. Ce mécanisme permet l'interopérabilité entre [[multi-ledger|ledgers]] indépendants (ex: main, gaming, nft) tout en garantissant la conservation de la masse monétaire totale grâce à un système anti-replay persistant dans RocksDB.

## Dates

| | Date |
|---|---|
| Créée | 2026-02-13 |
| Dernière mise à jour | 2026-03-13 |
| Version d'introduction | v0.1.0 |

Le commit initial est `b207bed` (*feat: add bridge, compliance, cube, wallet-factory modules and multi-token enhancements*). Les fichiers du bridge ont été touchés par des commits subséquents (clippy, fmt, LRU cache, TPS optimisations) jusqu'au 2026-03-13.

## Configuration

### Paramètres dans `Settings.fees` (`config.toml`)

| Paramètre | Type | Défaut | Description |
|-----------|------|--------|-------------|
| `cross_ledger_fee_multiplier` | `f64` | `2.0` | Multiplicateur appliqué au fee de base pour les transferts cross-ledger. `0.0` = pas de frais supplémentaire. |

Le multiplicateur est appliqué dans le handler `admin_bridge_transfer` : le fee de base de la transaction est calculé via `fee_policy.compute_fee(&req.amount)`, puis multiplié par `cross_ledger_fee_multiplier`. Le résultat est émis comme un bloc `Reward` sur le ledger principal.

### Pré-requis

- **[[multi-ledger|Multi-ledger]] actif** : `LedgerManager` doit être initialisé (champ `ledger_mgr` dans `AppState`). Sans cela, tous les endpoints bridge retournent `503 Service Unavailable`.
- **Coordinator wallet** : Les blocs `BridgeLock` et `BridgeMint` ne peuvent être signés que par le Coordinator (`coordinator_public_key` dans `validation`). Le `BridgeEngine` utilise le `node_wallet` (wallet du coordinateur) pour signer les blocs.
- **Bridge link actif** : Un pont doit être explicitement activé via `POST /admin/bridge/enable` avant qu'un transfert puisse s'effectuer. La direction (`Bidirectional`, `AtoB`, `BtoA`) contrôle le sens autorisé.

### BridgeLink — Configuration du pont

Un `BridgeLink` est l'objet stocké dans RocksDB qui définit un pont entre deux ledgers :

```rust
pub struct BridgeLink {
    pub ledger_a: String,        // Premier ledger (tri alphabétique)
    pub ledger_b: String,        // Second ledger (tri alphabétique)
    pub direction: BridgeDirection, // Bidirectional | AtoB | BtoA
    pub enabled: bool,           // Actif ou coupé
    pub created_at: i64,         // Timestamp de création (ms)
    pub disabled_at: Option<i64>, // Timestamp de désactivation
    pub authorized_by: Vec<String>, // Clés publiques ayant autorisé le pont
}
```

La clé de stockage est normalisée par tri alphabétique des deux ledger IDs (`BridgeLink::storage_key("nft", "main")` donne `"main:nft"`), garantissant qu'un seul enregistrement existe par paire.

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-bridge` | `crates/pms-bridge/src/types.rs` | Types de données : `BridgeLink`, `BridgeDirection`, `BridgeTransferRequest`, `BridgeTransferResponse`, `BridgeEnableRequest`, `BridgeDisableRequest` |
| `pms-bridge` | `crates/pms-bridge/src/engine.rs` | Orchestration des opérations de pont : `BridgeEngine` (enable, disable, transfer, status) |
| `pms-bridge` | `crates/pms-bridge/src/store.rs` | Accès RocksDB : `BridgeStore` (CRUD `bridge_links`, anti-replay `bridge_consumed`) |
| `pms-bridge` | `crates/pms-bridge/src/auth.rs` | Logique d'autorisation : `BridgeAuth` (qui peut enable/disable/transfer) |
| `pms-bridge` | `crates/pms-bridge/src/lib.rs` | Ré-exports publics du crate |
| `pms-server` | `crates/pms-server/src/api_fn/bridge.rs` | Handlers HTTP (Axum) pour les 5 endpoints bridge |
| `pms-server` | `crates/pms-server/src/api.rs` | Montage des routes bridge dans le routeur global |
| `pms-types-payload` | `crates/pms-types-payload/src/payload.rs` | Définition des variantes `PlainPayload::BridgeLock` et `PlainPayload::BridgeMint` |
| `pms-core` | `crates/pms-core/src/validations/check.rs` | Validation synchrone des blocs BridgeLock/BridgeMint (signature Coordinator, champs requis) |
| `pms-core` | `crates/pms-core/src/validations/transactions.rs` | Validation asynchrone des inputs BridgeLock (UTXO existence, somme, asset matching) |
| `pms-core` | `crates/pms-core/src/validations/apply.rs` | Application RAM : marque les inputs comme dépensés pour BridgeLock |
| `pms-core` | `crates/pms-core/src/net_adapter.rs` | `CoreAdapter::persist_block()` : validation UTXO + calcul du `UtxoDelta` pour BridgeLock/BridgeMint |
| `pms-wallet` | `crates/pms-wallet/src/history.rs` | Détection d'adresses dans les payloads bridge pour l'indexation historique |
| `pms-storage` | `crates/pms-storage/src/helpers.rs` | Extraction d'adresses pour l'indexation d'[[activity-system|activité]] (BridgeLock, BridgeMint) |
| `pms-server` | `crates/pms-server/src/api_fn/activity.rs` | Classification d'activité : types `bridge_lock_in` et `bridge_mint` |
| `pms-config` | `crates/pms-config/src/config.rs` | Paramètre `cross_ledger_fee_multiplier` dans `FeesSettings` |
| `pms-bridge` | `crates/pms-bridge/tests/bridge_test.rs` | Tests unitaires : types, auth, store CRUD, sérialisation, payloads |
| `pms-bridge` | `crates/pms-bridge/tests/bridge_e2e_test.rs` | Tests end-to-end : lifecycle complet, direction, balance insuffisante, transfers multiples |

## Fonctions Clés

### Crate `pms-bridge`

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `BridgeEngine::new()` | `engine.rs` | Construit le moteur bridge à partir du `LedgerManager`, du `BridgeStore`, et du wallet coordinateur |
| `BridgeEngine::enable_bridge()` | `engine.rs` | Active un pont entre deux ledgers après vérification des autorisations et de l'existence des ledgers |
| `BridgeEngine::disable_bridge()` | `engine.rs` | Désactive un pont (soft-delete : `enabled = false`, `disabled_at` set) |
| `BridgeEngine::list_bridges()` | `engine.rs` | Liste tous les ponts (actifs et inactifs) |
| `BridgeEngine::transfer_status()` | `engine.rs` | Retourne l'ID du bloc `BridgeMint` associé à un `lock_block_id` donné (ou `None`) |
| `BridgeEngine::execute_transfer()` | `engine.rs` | **Fonction principale** : exécute le transfert cross-ledger en 7 étapes (voir Mécanisme ci-dessous) |
| `BridgeEngine::build_and_sign_block()` | `engine.rs` | Construit un `WireBlock` signé par le wallet coordinateur avec PoW si requis |
| `BridgeLink::storage_key()` | `types.rs` | Normalise la clé de stockage par tri alphabétique des IDs de ledger |
| `BridgeLink::allows_transfer()` | `types.rs` | Vérifie si un transfert `from -> to` est autorisé par ce lien (direction + enabled) |
| `BridgeAuth::can_enable()` | `auth.rs` | Vérifie si l'appelant peut activer un pont (admin toujours, owner d'au moins un ledger pour custom-custom) |
| `BridgeAuth::can_disable()` | `auth.rs` | Vérifie si l'appelant peut désactiver un pont (admin toujours, owner d'un des deux ledgers) |
| `BridgeAuth::can_transfer()` | `auth.rs` | Vérifie si l'appelant peut transférer via un pont (admin toujours, owner du ledger source) |
| `BridgeAuth::requires_admin()` | `auth.rs` | Détecte si un des deux ledgers est admin-owned (`owner_pubkey == None`) |
| `BridgeStore::set_bridge_link()` | `store.rs` | Écrit/met à jour un `BridgeLink` dans la CF `bridge_links` |
| `BridgeStore::get_bridge_link()` | `store.rs` | Lit un `BridgeLink` depuis la CF `bridge_links` |
| `BridgeStore::is_bridge_enabled()` | `store.rs` | Vérifie si un pont actif autorise le transfert dans la direction donnée |
| `BridgeStore::disable_bridge_link()` | `store.rs` | Désactive un pont (set `enabled = false`, `disabled_at = now`) |
| `BridgeStore::list_bridge_links()` | `store.rs` | Itère sur toute la CF `bridge_links` et retourne tous les liens |
| `BridgeStore::is_bridge_lock_consumed()` | `store.rs` | Vérifie si un `lock_block_id` a déjà été consommé (anti-replay) |
| `BridgeStore::mark_bridge_lock_consumed()` | `store.rs` | Marque un lock comme consommé en associant `lock_block_id -> mint_block_id` dans la CF `bridge_consumed` |
| `BridgeStore::get_bridge_mint_for_lock()` | `store.rs` | Retourne le `mint_block_id` associé à un lock (pour le statut) |

### Crate `pms-core` (Validation)

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `check_block()` (BridgeLock branch) | `validations/check.rs` | Vérifie que le BridgeLock est signé par le Coordinator, a au moins un input, et a `dest_ledger_id` + `dest_address` non-vides |
| `check_block()` (BridgeMint branch) | `validations/check.rs` | Vérifie que le BridgeMint est signé par le Coordinator, a au moins un output, et a `lock_block_id` + `source_ledger_id` non-vides |
| `validate_bridge_lock_async()` | `validations/transactions.rs` | Validation UTXO asynchrone : pas de doublons internes, tous les inputs existent, `sum(inputs) >= amount`, asset_id cohérent |
| `apply_block()` (BridgeLock branch) | `validations/apply.rs` | Marque les inputs comme dépensés dans le DAG RAM (`mark_spent_ram`) |
| `compute_utxo_delta()` (BridgeLock) | `net_adapter.rs` | `UtxoDelta { spend: inputs, create: [] }` -- les fonds quittent le ledger |
| `compute_utxo_delta()` (BridgeMint) | `net_adapter.rs` | `UtxoDelta { spend: [], create: outputs }` -- les fonds arrivent sur le ledger |

### Crate `pms-server` (Handlers HTTP)

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `bridge_engine()` | `api_fn/bridge.rs` | Helper : construit un `BridgeEngine` depuis l'`AppState` (LedgerManager + default store + node wallet) |
| `admin_bridge_enable()` | `api_fn/bridge.rs` | Handler `POST /admin/bridge/enable` -- active un pont (admin-only) |
| `admin_bridge_disable()` | `api_fn/bridge.rs` | Handler `POST /admin/bridge/disable` -- désactive un pont (admin-only) |
| `admin_bridge_transfer()` | `api_fn/bridge.rs` | Handler `POST /admin/bridge/transfer` -- exécute un transfert + applique le [[economics|cross-ledger fee]] |
| `list_bridge_links()` | `api_fn/bridge.rs` | Handler `GET /v1/bridge/links` -- liste tous les ponts (public) |
| `bridge_status()` | `api_fn/bridge.rs` | Handler `GET /v1/bridge/status/{lock_block_id}` -- statut d'un transfert (public) |

## Endpoints API

### Routes Admin (protégées par admin token + IP allowlist)

| Méthode | Path | Description | Code Succès |
|---------|------|-------------|-------------|
| POST | `/admin/bridge/enable` | Active un pont entre deux ledgers | 201 |
| POST | `/admin/bridge/disable` | Désactive un pont entre deux ledgers | 200 |
| POST | `/admin/bridge/transfer` | Exécute un transfert cross-ledger (lock-and-mint) | 201 |

### Routes Publiques (lecture seule)

| Méthode | Path | Description | Code Succès |
|---------|------|-------------|-------------|
| GET | `/v1/bridge/links` | Liste tous les ponts configurés (actifs et inactifs) | 200 |
| GET | `/v1/bridge/status/{lock_block_id}` | Statut d'un transfert par son lock_block_id | 200 / 404 |

### Payloads DAG

| Variante | Ledger | Direction | Description |
|----------|--------|-----------|-------------|
| `PlainPayload::BridgeLock` | Source | Sortant | Verrouille (détruit) les UTXOs sur le ledger source. Champs : `inputs`, `amount`, `asset_id`, `dest_ledger_id`, `dest_address` |
| `PlainPayload::BridgeMint` | Destination | Entrant | Crée les UTXOs sur le ledger destination. Champs : `outputs`, `lock_block_id`, `source_ledger_id` |

## RocksDB Column Families

| Column Family | Clé | Valeur | Description |
|---------------|-----|--------|-------------|
| `bridge_links` | `"{ledger_a}:{ledger_b}"` (tri alphabétique) | JSON `BridgeLink` | Stocke les configurations de pont entre paires de ledgers |
| `bridge_consumed` | `"{lock_block_id}"` | `"{mint_block_id}"` (UTF-8) | Table anti-replay : associe chaque BridgeLock consommé à son BridgeMint correspondant |

Les deux CFs sont déclarées dans `crates/pms-storage/src/rocks_store/store.rs` dans la liste `CF_NAMES` (et la liste hardcodée dans `new()`). Elles sont créées dans le `RocksStore` du default ledger et partagées globalement (pas per-ledger).

## Mécanisme de Transfert (execute_transfer)

Le transfert cross-ledger s'effectue en 7 étapes atomiques :

1. **Vérification du bridge link** : vérifie que le pont est actif et autorise la direction `from -> to` (`is_bridge_enabled`).
2. **Résolution des ledgers** : récupère les instances `LedgerInstance` source et destination depuis le `LedgerManager`.
3. **Coin selection** : sélectionne les UTXOs du `from_address` sur le ledger source, filtre par `asset_id`, tri largest-first, et accumule jusqu'à couvrir le montant. Échoue si `sum(selected) < amount`.
4. **BridgeLock** : construit et persiste un bloc `BridgeLock` signé par le Coordinator sur le ledger source. Ce bloc consomme les UTXOs sélectionnés (ils sont marqués comme dépensés).
5. **Anti-replay check** : vérifie que le `lock_block_id` n'a pas déjà été consommé dans `bridge_consumed`.
6. **BridgeMint** : construit et persiste un bloc `BridgeMint` signé par le Coordinator sur le ledger destination. Ce bloc crée un UTXO pour `to_address` avec le montant transféré.
7. **Marquage anti-replay** : enregistre `lock_block_id -> mint_block_id` dans la CF `bridge_consumed`.

### Diagramme de flux

```
Ledger Source                              Ledger Destination
     |                                           |
     |  1. BridgeLock (consomme UTXOs)           |
     |  [inputs: UTXO_A, UTXO_B]                |
     |  [amount: "100.00000000"]                 |
     |  [dest_ledger_id: "nft"]                  |
     |  [dest_address: "8e1..."]                 |
     |                                           |
     |           bridge_consumed                 |
     |         lock_id -> mint_id                |
     |                                           |
     |                                           |  2. BridgeMint (crée UTXOs)
     |                                           |  [outputs: {addr, amount}]
     |                                           |  [lock_block_id: "..."]
     |                                           |  [source_ledger_id: "main"]
     |                                           |
```

## Sécurité

### Signature Coordinator-Only

Les blocs `BridgeLock` et `BridgeMint` sont validés uniquement s'ils sont signés par le Coordinator (`coordinator_public_key`). Cette vérification est effectuée dans `check_block()` (validation synchrone). Si la clé du signataire ne correspond pas, le bloc est rejeté avec `ValidationError::InvalidSignature`.

### Anti-Replay

La CF `bridge_consumed` empêche un `BridgeLock` d'être utilisé deux fois pour créer des fonds. `mark_bridge_lock_consumed()` échoue si l'entrée existe déjà (double-check en base avant écriture).

### Validation UTXO

`validate_bridge_lock_async()` vérifie :
- Pas de doublons internes dans les inputs
- Chaque input existe dans l'UTXO set
- L'`asset_id` de chaque input correspond à celui du transfert
- La somme des inputs est >= au montant demandé

### Vérification de gel ([[compliance|Compliance]])

Avant de persister un `BridgeLock`, `CoreAdapter::persist_block()` vérifie que les adresses des UTXOs sources ne sont pas gelées (`is_frozen()`). Un compte gelé ne peut pas initier de transfert cross-ledger.

### Autorisation

- **Ledger admin-owned** (pas de `owner_pubkey`) : seul l'admin peut gérer les ponts impliquant ce ledger.
- **Ledger custom-custom** : le owner d'au moins un des deux ledgers peut activer un pont ; le owner d'un seul peut le désactiver (pour se protéger).
- **Transfert** : seul le owner du ledger source peut initier un transfert (ou l'admin).

## Interactions

### [[multi-ledger|Multi-Ledger]]

Le bridge repose entièrement sur le système multi-ledger (`LedgerManager`). Chaque ledger a son propre DAG, UTXO set, et `RocksStore` indépendant. Le `BridgeStore` utilise le store du default ledger pour stocker les CFs globales `bridge_links` et `bridge_consumed`.

### Frais ([[economics|Economics]])

Lors d'un transfert cross-ledger via `admin_bridge_transfer`, un frais supplémentaire est calculé :

```
cross_fee = base_fee(amount) * cross_ledger_fee_multiplier
```

Ce frais est émis comme un bloc `Reward` sur le ledger principal via `create_reward_block()`. Le multiplicateur par défaut est `2.0`, configurable via `fees.cross_ledger_fee_multiplier` dans le fichier de configuration.

### [[activity-system|Activité Wallet]]

Les opérations bridge sont indexées dans le système d'activité wallet :
- **`bridge_lock_in`** : visible pour le `dest_address` dans le BridgeLock (direction "in")
- **`bridge_mint`** : visible pour chaque output du BridgeMint (direction "in")

Ces types appartiennent à la catégorie `ActivityCategory::Bridge` (index `7`), utilisable comme filtre dans les queries d'activité.

### Historique Wallet

Le module `pms-wallet/history.rs` sait extraire les adresses impliquées dans les payloads bridge :
- `BridgeLock` : `dest_address`
- `BridgeMint` : toutes les adresses des `outputs`

Cela permet aux queries d'historique de retrouver les transactions bridge pour une adresse donnée.

### Tokens Custom (Multi-Asset)

Le bridge supporte les tokens custom via le champ `asset_id` dans `BridgeTransferRequest`. Quand `asset_id` est `None`, le token natif PMS est transféré. Le coin selection filtre les UTXOs par `asset_id` pour ne sélectionner que ceux du bon asset.

## Tests

| Fichier | Test | Description |
|---------|------|-------------|
| `bridge_test.rs` | `storage_key_is_sorted` | La clé de stockage normalise l'ordre alphabétique |
| `bridge_test.rs` | `bridge_link_allows_transfer_bidirectional` | Un lien bidirectionnel autorise les deux sens |
| `bridge_test.rs` | `bridge_link_allows_transfer_directional` | AtoB/BtoA n'autorisent qu'un sens |
| `bridge_test.rs` | `bridge_link_disabled_blocks_transfer` | Un lien désactivé bloque tout transfert |
| `bridge_test.rs` | `auth_admin_can_always_manage` | L'admin peut tout faire (enable/disable/transfer) |
| `bridge_test.rs` | `auth_non_admin_cannot_manage_main_bridge` | Un non-admin ne peut pas gérer un pont impliquant main |
| `bridge_test.rs` | `auth_owner_can_manage_custom_bridges` | Le owner peut gérer les ponts entre ledgers custom |
| `bridge_test.rs` | `store_bridge_link_crud` | CRUD complet sur BridgeStore (RocksDB) |
| `bridge_test.rs` | `store_disable_bridge_link` | Désactivation d'un pont + vérification du timestamp |
| `bridge_test.rs` | `store_anti_replay` | Anti-replay : mark consumed, verify consumed, double-consume fails |
| `bridge_test.rs` | `bridge_link_serialization_roundtrip` | Sérialisation/désérialisation JSON de BridgeLink |
| `bridge_test.rs` | `bridge_direction_serialization` | Sérialisation des variantes BridgeDirection |
| `bridge_test.rs` | `bridge_lock_payload_serialization` | Sérialisation roundtrip du payload BridgeLock |
| `bridge_test.rs` | `bridge_mint_payload_serialization` | Sérialisation roundtrip du payload BridgeMint |
| `bridge_e2e_test.rs` | `bridge_full_lifecycle` | Cycle complet : mint -> enable -> transfer -> verify UTXOs -> status -> anti-replay -> disable -> re-enable -> transfer |
| `bridge_e2e_test.rs` | `bridge_directional_atob` | Contrainte directionnelle : A->B passe, B->A échoue |
| `bridge_e2e_test.rs` | `bridge_insufficient_balance` | Balance insuffisante échoue proprement |
| `bridge_e2e_test.rs` | `bridge_multiple_transfers` | Transferts multiples accumulent correctement sur la destination |
