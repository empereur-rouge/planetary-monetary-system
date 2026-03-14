---
tags: [feature]
created: 2025-12-28
updated: 2026-03-13
version: v0.1.0
---

# Multi-Ledger

## Résumé

Le système multi-ledger permet d'exécuter N instances de DAG complètement isolées au sein d'un même processus moteur, en partageant une seule base RocksDB physique. Chaque ledger dispose de son propre DAG en RAM, de son propre jeu UTXO (256 shards), de son propre genesis block, et de column families RocksDB préfixées. Cela permet de faire tourner un ledger principal ("main") et des ledgers secondaires (gaming, NFT, sidechains clients) sur une seule instance sans interférences de données, tout en offrant un pont cross-ledger ([[bridge]]) pour les transferts d'actifs entre ledgers.

## Dates

| | Date |
|---|---|
| Créée | 2025-12-28 |
| Dernière mise à jour | 2026-03-13 |
| Version d'introduction | v0.1.0 |

Notes :
- Le crate `pms-ledger` a été introduit initialement le 2025-12-28 (`feat: Introduce initial implementations`).
- Les structs `LedgerInstance`, `LedgerManager`, et l'API admin ont été structurés le 2026-02-12 dans le commit `update: major-rework/centralizing`.
- La dernière mise à jour majeure date du 2026-03-13 (`fix(storage): ensure_column_families at bootstrap`).

## Architecture

### Isolation des ledgers

Chaque ledger est une instance autonome encapsulée par `LedgerInstance` :

```
LedgerInstance {
    id: String,                         // "main", "gaming", "nft"
    dag: Arc<ConcurrentDag>,            // DAG concurrent lock-free en RAM (propre au ledger)
    utxos: Arc<ShardedUtxoSet>,         // Cache UTXO partitionné (256 shards, propre au ledger)
    store: Arc<RocksStore>,             // Store RocksDB avec prefix isolé
    adapter: Arc<dyn NetDagAdapter>,    // Adapter core (persist_block, top_tips, etc.)
    def: LedgerDef,                     // Config du ledger (network_id, protocol_version, fees, etc.)
}
```

### Base de données partagée

Tous les ledgers partagent une **seule instance RocksDB** (`Arc<PmsDb>`), ouverte par `RocksStore::open_db_multi_prefix()`. L'isolation est assurée par des **column families préfixées** :

```
prefix:blocks        // ex: "main:blocks", "nft:blocks", "gam:blocks"
prefix:utxo
prefix:tips
prefix:nft_ownership
prefix:contracts
prefix:gas_pools
prefix:ledger_subscriptions
... (32 CFs par prefix)
```

La liste complète des CFs est définie dans `RocksStore::CF_NAMES` (32 column families par ledger).

### Bootstrap

Au démarrage, `LedgerManager::bootstrap()` :

1. Appelle `Settings::effective_ledgers()` pour obtenir les définitions de ledgers (depuis `[[ledgers]]` TOML ou génération automatique d'un ledger "main" en rétrocompatibilité).
2. Ouvre la DB partagée avec `open_db_multi_prefix()` en créant toutes les CFs pour tous les préfixes.
3. Pour chaque `LedgerDef`, crée un `LedgerInstance::bootstrap()` qui :
   - Crée un `RocksStore::from_shared_db()` avec le prefix du ledger.
   - Assure les CFs (`ensure_column_families()`), le schéma, et le cache des adresses gelées.
   - Crée le genesis block si la DB est vide pour ce prefix.
   - Vérifie la compatibilité de la version DAG (`check_dag_compatibility()`).
   - Charge le DAG en RAM depuis RocksDB (avec capacité bornée par `max_dag_blocks`).
   - Streame les UTXOs depuis RocksDB vers le cache en mémoire (canal borné de 10 000 éléments pour éviter les OOM).

### Thread safety

Le `LedgerManager` utilise un `DashMap<String, Arc<LedgerInstance>>` pour un accès concurrent sans lock global. Les ledgers peuvent être ajoutés dynamiquement à chaud via `add_ledger()`.

## Configuration

### Mode automatique (rétrocompatible)

Si aucune section `[[ledgers]]` n'est présente dans le TOML, un seul ledger "main" est créé automatiquement à partir des sections `[rocks]` et `[network]` :

```toml
[rocks]
path = "/data/pms"
prefix = "pms:main"

[network]
network_id = "pms-mainnet"
protocol_version = 1
symbol = "PMS"
```

### Mode multi-ledger explicite

```toml
[[ledgers]]
id = "main"
network_id = "pms-mainnet"
prefix = "pms:main"
protocol_version = 1
symbol = "PMS"

[[ledgers]]
id = "gaming"
network_id = "pms-gaming"
prefix = "gam"
protocol_version = 1
tip_limit = 64
symbol = "GAME"

[ledgers.fees]
ratio = "0.01"
base_fee = "0.5"
gas_per_tx = "0.002"
gas_pool_min_balance = "20.0"

[ledgers.validation]
max_inputs = 5
max_outputs = 5
```

### Champs de `LedgerDef`

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `id` | `String` | oui | Identifiant unique du ledger |
| `network_id` | `String` | oui | ID réseau pour le routage P2P |
| `prefix` | `String` | oui | Prefix des column families RocksDB |
| `protocol_version` | `u32` | non | Version du protocole P2P (défaut: 1) |
| `tip_limit` | `Option<usize>` | non | Limite de tips (hérite du global si absent) |
| `fees` | `Option<LedgerFeesOverride>` | non | Surcharges de fees (hérite du global si absent) |
| `validation` | `Option<LedgerValidationOverride>` | non | Surcharges de validation |
| `owner_pubkey` | `Option<String>` | non | Clé publique du propriétaire (`None` = admin-owned) |
| `symbol` | `Option<String>` | non | Symbole du token natif (défaut: "PMS") |

### Surcharges de fees (`LedgerFeesOverride`)

Chaque champ à `None` hérite de la config globale `[fees]`. Les champs supportés incluent : `ratio`, `base_fee`, `platform_fee_ratio`, `block_reward`, `fee_tiers`, `mint_fee_base`, `mint_fee_ratio`, `token_creation_fee`, `nft_mint_fee`, `nft_fee_exempt_types`, `fee_distribution`, `treasury_fee_percent`, `coordinator_fee_percent`, `burn_rate_bps`, `gas_per_tx`, `gas_pool_min_balance`, `contract_deployment_fee`, `storage_fee_per_kb`.

La résolution se fait via `resolve_effective_fees(global, ledger_override)` qui merge les deux couches.

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-ledger` | `crates/pms-ledger/src/lib.rs` | Réexporte `LedgerInstance`, `LedgerManager`, et le module picks |
| `pms-ledger` | `crates/pms-ledger/src/instance.rs` | Définition de `LedgerInstance` et logique de bootstrap |
| `pms-ledger` | `crates/pms-ledger/src/manager.rs` | Définition de `LedgerManager` (DashMap, bootstrap, add_ledger, lookups) |
| `pms-ledger` | `crates/pms-ledger/src/picks.rs` | Sélection des adresses de fee recipients |
| `pms-config` | `crates/pms-config/src/config.rs` | Définition de `LedgerDef`, `LedgerFeesOverride`, `LedgerValidationOverride` |
| `pms-config` | `crates/pms-config/src/settings.rs` | `Settings.ledgers` et `effective_ledgers()` (génération automatique du ledger "main") |
| `pms-server` | `crates/pms-server/src/api.rs` | `AppState.ledger_mgr`, `dynamic_ledger_handler()`, `build_ledger_scoped_routes()` |
| `pms-server` | `crates/pms-server/src/api_fn/ledger.rs` | Endpoints API : `list_ledgers`, `admin_list_ledgers`, `admin_get_ledger`, `admin_create_ledger` |
| `pms-server` | `crates/pms-server/src/api_fn/tx_helpers.rs` | `EffectiveFees`, `resolve_effective_fees()`, `try_consume_gas()` |
| `pms-server` | `crates/pms-server/src/server.rs` | `Server.ledger_manager()`, `adapter_for_network()` (routage P2P multi-ledger) |
| `pms-storage` | `crates/pms-storage/src/rocks_store/store.rs` | `open_db_multi_prefix()`, `from_shared_db()`, `CF_NAMES`, `build_cf_names()` |
| `pms-bridge` | `crates/pms-bridge/src/engine.rs` | `BridgeEngine` : pont cross-ledger (lock/mint) via `LedgerManager` |
| `pms-storage` | `crates/pms-storage/src/gas_pool_store.rs` | Trait `GasPoolStorage` : CRUD des gas pools par ledger |
| `pms-storage` | `crates/pms-storage/src/rocks_store/gas_pool_storage.rs` | Implémentation RocksDB du gas pool storage |
| `bin` | `bin/src/main.rs` | Bootstrap du `LedgerManager` et passage au `Server` |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `LedgerInstance::bootstrap()` | `crates/pms-ledger/src/instance.rs` | Bootstrap complet d'un ledger (store, genesis, DAG, UTXOs) |
| `LedgerManager::bootstrap()` | `crates/pms-ledger/src/manager.rs` | Ouvre la DB partagée et bootstrap tous les ledgers définis |
| `LedgerManager::add_ledger()` | `crates/pms-ledger/src/manager.rs` | Création dynamique d'un ledger à chaud (CFs, genesis, DAG, UTXOs) |
| `LedgerManager::get()` | `crates/pms-ledger/src/manager.rs` | Lookup d'un ledger par ID |
| `LedgerManager::get_by_network_id()` | `crates/pms-ledger/src/manager.rs` | Lookup d'un ledger par network_id (routage P2P) |
| `LedgerManager::default_ledger()` | `crates/pms-ledger/src/manager.rs` | Retourne le ledger "main" (ou le premier disponible) |
| `LedgerManager::list_all()` | `crates/pms-ledger/src/manager.rs` | Liste toutes les instances de ledgers actifs |
| `Settings::effective_ledgers()` | `crates/pms-config/src/settings.rs` | Génère les définitions de ledgers (rétrocompatible si aucun `[[ledgers]]`) |
| `RocksStore::open_db_multi_prefix()` | `crates/pms-storage/src/rocks_store/store.rs` | Ouvre RocksDB avec les CFs de N préfixes en une seule DB |
| `RocksStore::from_shared_db()` | `crates/pms-storage/src/rocks_store/store.rs` | Crée un `RocksStore` pointé vers une DB partagée avec un prefix |
| `RocksStore::build_cf_names()` | `crates/pms-storage/src/rocks_store/store.rs` | Pré-calcule la map `short_name -> prefix:name` (évite `format!()` sur le hot path) |
| `dynamic_ledger_handler()` | `crates/pms-server/src/api.rs` | Handler Axum qui résout le ledger et forwarde la requête |
| `build_ledger_scoped_routes()` | `crates/pms-server/src/api.rs` | Construit les routes spécifiques à un ledger (wallet, blocks, NFT, etc.) |
| `resolve_effective_fees()` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Merge les fees globales avec les overrides per-ledger |
| `try_consume_gas()` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Consomme du gas depuis le pool d'un ledger custom (non-"main") |
| `Server::api_only()` | `crates/pms-server/src/server.rs` | Crée un Server sans P2P pour le routage per-ledger |
| `Server::adapter_for_network()` | `crates/pms-server/src/server.rs` | Résout l'adapter d'un ledger par network_id pour le P2P |
| `BridgeEngine::execute_transfer()` | `crates/pms-bridge/src/engine.rs` | Exécute un transfert cross-ledger (lock sur source, mint sur destination) |

## Endpoints API

### Endpoints publics

| Méthode | Path | Description |
|---------|------|-------------|
| `GET` | `/v1/ledgers` | Liste tous les ledgers actifs (avec filtre `?search=`) |
| `GET` | `/v1/gas-pool/{ledger_id}` | Consulte le [[economics|gas pool]] d'un ledger |

### Endpoints admin

| Méthode | Path | Description |
|---------|------|-------------|
| `GET` | `/admin/ledgers` | Liste détaillée des ledgers (infos techniques) |
| `GET` | `/admin/ledgers/{ledger_id}` | Détail d'un ledger spécifique |
| `POST` | `/admin/ledgers/create` | Crée un nouveau ledger dynamiquement |
| `POST` | `/admin/gas-pool/deposit` | Dépose des PMS dans le gas pool d'un ledger |
| `POST` | `/admin/gas-pool/withdraw` | Retire des PMS du gas pool d'un ledger |

### Endpoints [[bridge]] (cross-ledger)

| Méthode | Path | Description |
|---------|------|-------------|
| `POST` | `/admin/bridge/enable` | Active un pont entre deux ledgers |
| `POST` | `/admin/bridge/disable` | Désactive un pont entre deux ledgers |
| `POST` | `/admin/bridge/transfer` | Exécute un transfert cross-ledger |
| `GET` | `/v1/bridge/links` | Liste tous les ponts actifs |
| `GET` | `/v1/bridge/status/{lock_block_id}` | Statut d'un transfert bridge |

### Métriques

| Méthode | Path | Description |
|---------|------|-------------|
| `GET` | `/metrics` | Métriques du ledger par défaut |
| `GET` | `/metrics/all` | Métriques complètes (tous les ledgers, avec labels) |
| `GET` | `/l/{ledger_id}/metrics` | Métriques d'un ledger spécifique |

## URL Routing

### Routes sans prefix (ledger par défaut)

Les routes sans prefix `/l/` pointent vers le ledger par défaut ("main"). L'`AppState` est initialisé avec `ledger_id: "main"` et le store/adapter du ledger principal.

```
GET  /v1/supply              -> ledger "main"
POST /v1/tx/prepare          -> ledger "main"
POST /v1/wallet/send-simple  -> ledger "main"
```

### Routes per-ledger (`/l/{ledger_id}/...`)

Toutes les routes ledger-scoped sont accessibles sous `/l/{ledger_id}/` grâce au handler dynamique :

```
Route pattern : /l/{ledger_id}/{*rest}
Handler       : dynamic_ledger_handler()
```

Le handler dynamique :

1. Extrait `ledger_id` et `rest` du path.
2. Résout le ledger depuis le `LedgerManager` (retourne 404 si inconnu).
3. Construit un `AppState` per-ledger avec :
   - Un `Server::api_only()` utilisant l'adapter du ledger.
   - Le `store` du ledger (RocksStore avec le bon prefix).
   - Le `ledger_id` pour le contexte.
   - Les `effective_fees` résolues (merge global + overrides du ledger).
4. Reconstruit la requête en supprimant le prefix `/l/{ledger_id}`.
5. Forwarde via `Router::oneshot()`.

Exemples :

```bash
# Balance sur le ledger "gaming"
POST /l/gaming/v1/balance

# Tips du DAG "gaming"
POST /l/gaming/v1/dag/tips

# Supply du ledger "gaming"
GET  /l/gaming/v1/supply

# Mint NFT sur le ledger "nft"
POST /l/nft/v1/nft/mint

# Admin: faucet sur un ledger spécifique
POST /l/gaming/admin/faucet
```

Les ledgers créés dynamiquement (via `POST /admin/ledgers/create`) sont accessibles **immédiatement** sans redémarrage du serveur.

### Routage P2P

Le serveur P2P route les blocs entrants vers le bon ledger via `adapter_for_network(network_id)`, qui consulte le `LedgerManager::get_by_network_id()`. Si aucun match, le bloc est routé vers l'adapter par défaut (ledger "main").

## Interactions

### [[bridge|Bridge]] (Cross-Ledger)

Le `BridgeEngine` (`crates/pms-bridge/src/engine.rs`) orchestre les transferts cross-ledger :

1. **Enable** : Active un pont bidirectionnel entre deux ledgers (nécessite les permissions admin ou owner).
2. **Transfer** :
   - Sélectionne les UTXOs sur le ledger source.
   - Crée un bloc `BridgeLock` sur le ledger source (consomme les UTXOs).
   - Crée un bloc `BridgeMint` sur le ledger destination (crée de nouveaux UTXOs).
   - Marque le lock comme consommé (anti-replay via `bridge_consumed` CF).
3. **Disable** : Désactive un pont.

Le bridge utilise directement le `LedgerManager` pour accéder aux adapters des deux ledgers impliqués.

### [[economics|Gas Pool]] (Anti-Spam)

Chaque ledger custom (non-"main") possède un **gas pool** (CF `gas_pools`) qui doit être approvisionné pour autoriser les transactions. À chaque transaction sur un ledger custom, `try_consume_gas()` est appelé pour déduire le `gas_per_tx` du pool. Si le pool est épuisé, la transaction est rejetée avec HTTP 402 (Payment Required).

- Le gas pool est auto-créé lors de la création d'un ledger via `admin_create_ledger`.
- Les admins gèrent le pool via `POST /admin/gas-pool/deposit` et `POST /admin/gas-pool/withdraw`.
- Le ledger "main" n'a **jamais** de gas pool (exempt par design).

### Subscriptions (Ledger Annuels)

Chaque ledger custom peut avoir une subscription annuelle (CF `ledger_subscriptions`). La subscription contrôle si le ledger est actif et autorisé à traiter des transactions.

### [[fee-distribution|Fee Distribution]]

La distribution de fees est contextualisée par `ledger_id` :

- Les métriques `BLOCKS_PERSISTED` et `PMS_BLOCKS_TOTAL` sont labélisées par ledger.
- Chaque ledger peut avoir ses propres ratios de fees via `LedgerFeesOverride`.
- La résolution des fees se fait via `EffectiveFees` qui merge la config globale avec les overrides per-ledger.

### Fees Cross-Ledger

Le paramètre `cross_ledger_fee_multiplier` (défaut: 2.0) s'applique aux transferts [[bridge]]. Les frais de transfert cross-ledger sont majorés par ce multiplicateur par rapport aux transferts intra-ledger.

### Métriques et Monitoring

- `PMS_BLOCKS_TOTAL` : jauge synchronisée par ledger (reflète le DAG en RAM après [[dag-pruning|pruning]]).
- `BLOCKS_PERSISTED` : compteur par ledger, initialisé au démarrage depuis la DB.
- `/metrics` : métriques du ledger par défaut (sans labels, compatible dashboard).
- `/metrics/all` : format Prometheus complet avec labels ledger (pour Grafana/ops).
- `/l/{ledger_id}/metrics` : métriques d'un ledger spécifique (sans labels).
