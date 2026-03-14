---
tags: [feature]
created: 2026-03-14
updated: 2026-03-14
version: v0.3.0
---

# Economics System (Fee Burn, Gas Pool, Dynamic Fees)

## Résumé

Le système Economics (v0.3.0) introduit six mécanismes économiques complémentaires pour transformer le réseau PMS d'un simple registre de transactions en un écosystème monétaire autosuffisant avec des leviers déflationnistes, anti-spam, et de tarification dynamique.

**Fee Burn** retire définitivement une fraction configurable des frais de transaction de la circulation, créant une pression déflationniste sur l'offre de PMS. Le taux est exprimé en basis points (bps) et appliqué lors de chaque distribution de fees par le Coordinator.

**Gas Pool** est un mécanisme anti-spam per-ledger. Chaque [[multi-ledger|ledger]] custom (non "main") possède un pool de gas qui doit être approvisionné en PMS. Chaque transaction sur un ledger custom consomme du gas depuis son pool. Quand le pool est épuisé (ou sous le seuil minimum), le ledger devient read-only jusqu'à réapprovisionnement.

**Dynamic Fees** ajuste automatiquement les frais de transaction en fonction de la congestion du réseau. Un `TpsTracker` mesure le nombre de blocs persistés dans une fenêtre glissante de 60 secondes. Quand le TPS dépasse le `target_tps` configuré, un multiplicateur (jusqu'à `max_fee_multiplier`) est appliqué aux frais.

**Storage Fees** ajoute un surcharge proportionnel à la taille du payload (en KB). Chaque KB de données (arrondi au supérieur) est facturé selon un taux configurable. Ce mécanisme est appliqué sur le mint de NFT (metadata payload).

**Cross-Ledger Fee** applique un multiplicateur sur les frais standard lors des transferts inter-ledgers via le [[bridge]]. Le multiplicateur par défaut est 2.0x.

**Contract Deployment Fee** facture un montant fixe en PMS lors de l'enregistrement d'un nouveau [[smart-contracts|contrat déclaratif]].

Tous ces mécanismes sont configurables à chaud via `RuntimeConfig` (ConfigUpdate) et supportent des overrides per-ledger via `LedgerFeesOverride`.

## Dates

| | Date |
|---|---|
| Créée | 2026-03-14 |
| Dernière mise à jour | 2026-03-14 |
| Version d'introduction | v0.3.0 |

## Sous-fonctionnalités

### 1. Fee Burn (Mécanisme déflationniste)

Lors de la [[fee-distribution|distribution automatique des fees]] (`perform_fee_distribution()`), une fraction `burn_rate_bps` du total des fees est retirée de la circulation avant distribution. Le montant brûlé est persisté dans RocksDB (`total_burned`) et exposé via `GET /v1/supply`.

- **Taux par défaut** : 0 bps (désactivé)
- **Taux recommandé** : 3000 bps (30%)
- **Plafond** : 10 000 bps (100%, cap automatique si dépassé)
- **Précision** : 8 décimales (`round_dp(8)`)

### 2. Gas Pool (Anti-spam per-ledger)

Chaque ledger custom possède un `GasPool` avec :
- `balance` : solde courant en PMS
- `total_consumed` : cumul du gas consommé (à vie)
- `total_deposited` : cumul des dépôts (à vie)
- `created_at` : timestamp de création

Le gas est consommé atomiquement (`read -> deduct -> write`) à chaque transaction. Si `balance - gas_per_tx < min_balance`, la transaction est rejetée avec `402 Payment Required`.

Le pool est auto-créé avec un solde de 0 lors de la création d'un nouveau ledger (`admin_create_ledger`).

### 3. Dynamic Fees (Tarification par congestion)

Le `TpsTracker` maintient une `VecDeque<Instant>` thread-safe (Mutex) de timestamps de blocs persistés dans une fenêtre glissante de 60 secondes.

Formule du multiplicateur :
```
multiplier = max(1.0, current_tps / target_tps)
```
Plafonné à `max_fee_multiplier` (défaut 5.0).

Le multiplicateur est calculé via `dynamic_fee_multiplier()` dans `tx_helpers.rs` et appliqué sur les frais de base des transactions.

### 4. Storage Fees (Surcharge par taille de payload)

Formule :
```
storage_fee = ceil(payload_bytes / 1024) * fee_per_kb
```
- Minimum 1 KB par transaction (même 1 byte coûte 1 KB)
- Appliqué sur le mint de NFT (taille de la metadata sérialisée en JSON)
- Retourne 0 si le payload est vide ou si `fee_per_kb` est 0

### 5. Cross-Ledger Fee (Surcharge inter-ledgers)

Lors d'un transfert via le [[bridge]] (`POST /admin/bridge/transfer`), les frais standards sont multipliés par `cross_ledger_fee_multiplier` (défaut 2.0x). Le fee résultant est distribué via un bloc Reward.

### 6. Contract Deployment Fee (Frais de déploiement)

Lors de l'enregistrement d'un nouveau [[smart-contracts|contrat]] (`POST /admin/contracts`), un montant fixe `contract_deployment_fee` est chargé. Le fee est distribué via un bloc Reward.

## Configuration

### Configuration TOML (`config.toml` - section `[fees]`)

```toml
[fees]
# Fee Burn — Percentage of fees permanently burned (basis points)
# 0 = disabled, 3000 = 30%, 10000 = 100%
burn_rate_bps = 3000

# Gas Pool — Per-ledger anti-spam
gas_per_tx = "0.001"            # Gas consumed per transaction on custom ledgers
gas_pool_min_balance = "10.0"   # Minimum pool balance before ledger goes read-only

# Dynamic Fees — Congestion-based multiplier
dynamic_fee_enabled = true
target_tps = 100                # Fees increase above this TPS
max_fee_multiplier = 5.0        # Maximum multiplier cap

# Storage Fees — Per-KB payload surcharge
storage_fee_per_kb = "0.01"     # PMS per KB of payload data

# Cross-Ledger Fee — Bridge transfer multiplier
cross_ledger_fee_multiplier = 2.0

# Contract Deployment Fee — One-time fee for registering a contract
contract_deployment_fee = "10.0"

# Ledger Subscription — Annual fee for custom ledgers (placeholder)
ledger_annual_fee_pms = "100.0"
```

### Per-Ledger Overrides (section `[[ledgers]]`)

```toml
[[ledgers]]
id = "my-custom-ledger"
network_id = "pms-custom"
prefix = "custom"

[ledgers.fees]
burn_rate_bps = 5000              # Override: 50% burn
gas_per_tx = "0.002"              # Override: higher gas cost
gas_pool_min_balance = "20.0"
contract_deployment_fee = "50.0"
storage_fee_per_kb = "0.05"
```

### RuntimeConfig Hot-Swap (via `POST /admin/config`)

Toutes les valeurs Economics sont modifiables à chaud via des transactions `ConfigUpdate` signées par le Coordinator :

```json
// SetBurnRate — Change the fee burn rate
{ "SetBurnRate": { "bps": 3000 } }

// SetContractDeploymentFee — Change deployment fee
{ "SetContractDeploymentFee": { "fee": "10.0" } }

// SetStorageFeePerKb — Change storage fee rate
{ "SetStorageFeePerKb": { "fee": "0.01" } }

// SetDynamicFee — Enable/configure dynamic fees
{
  "SetDynamicFee": {
    "enabled": true,
    "target_tps": 100,
    "max_multiplier": "5.0"
  }
}

// BatchUpdate — Multiple changes in one transaction
{
  "BatchUpdate": [
    { "SetBurnRate": { "bps": 3000 } },
    { "SetStorageFeePerKb": { "fee": "0.02" } },
    { "SetDynamicFee": { "enabled": true, "target_tps": 150, "max_multiplier": "3.0" } }
  ]
}
```

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-types-economics` | `crates/pms-types-economics/src/lib.rs` | Types partagés : `GasPool`, `FeeBurnResult`, `DynamicFeeInfo` |
| `pms-economics` | `crates/pms-economics/src/lib.rs` | Module racine, ré-export des sous-modules |
| `pms-economics` | `crates/pms-economics/src/fee_burn.rs` | Logique de calcul du burn (basis points) |
| `pms-economics` | `crates/pms-economics/src/gas_pool.rs` | Logique du gas pool : `consume()`, `deposit()`, `withdraw()`, `can_consume()` |
| `pms-economics` | `crates/pms-economics/src/dynamic_fee.rs` | `TpsTracker` : rolling window TPS + multiplicateur de congestion |
| `pms-economics` | `crates/pms-economics/src/storage_fee.rs` | Calcul du storage fee par KB |
| `pms-storage` | `crates/pms-storage/src/gas_pool_store.rs` | Trait `GasPoolStorage` : interface CRUD pour les gas pools |
| `pms-storage` | `crates/pms-storage/src/rocks_store/gas_pool_storage.rs` | Implémentation RocksDB de `GasPoolStorage` |
| `pms-storage` | `crates/pms-storage/src/rocks_store/node_rewards_storage.rs` | `increment_total_burned()` / `get_total_burned()` : persistance du cumul brûlé |
| `pms-storage` | `crates/pms-storage/src/rocks_store/store.rs` | CFs `gas_pools` et `ledger_subscriptions` dans `CF_NAMES` et `new()` |
| `pms-config` | `crates/pms-config/src/config.rs` | `FeesSettings` : champs `burn_rate_bps`, `gas_per_tx`, `gas_pool_min_balance`, `contract_deployment_fee`, `storage_fee_per_kb`, `dynamic_fee_enabled`, `target_tps`, `max_fee_multiplier`, `cross_ledger_fee_multiplier`, `ledger_annual_fee_pms` |
| `pms-config` | `crates/pms-config/src/config.rs` | `LedgerFeesOverride` : overrides per-ledger pour les champs economics |
| `pms-config` | `crates/pms-config/src/runtime.rs` | `RuntimeConfig` : champs `burn_rate_bps`, `contract_deployment_fee`, `storage_fee_per_kb`, `dynamic_fee_enabled`, `target_tps`, `max_fee_multiplier` |
| `pms-config` | `crates/pms-config/src/runtime.rs` | `ConfigUpdate` : variantes `SetBurnRate`, `SetContractDeploymentFee`, `SetStorageFeePerKb`, `SetDynamicFee` |
| `pms-server` | `crates/pms-server/src/fee_distribution.rs` | `perform_fee_distribution()` : intégration du burn dans la distribution des fees |
| `pms-server` | `crates/pms-server/src/api.rs` | `AppState.tps_tracker` : instance partagée du `TpsTracker` (60s window) |
| `pms-server` | `crates/pms-server/src/api_fn/gas_pool.rs` | Endpoints API gas pool : deposit, withdraw, get |
| `pms-server` | `crates/pms-server/src/api_fn/tx_helpers.rs` | `try_consume_gas()`, `dynamic_fee_multiplier()`, `load_storage_fee_per_kb()`, `load_burn_rate_bps()`, `load_contract_deployment_fee()`, `EffectiveFees` |
| `pms-server` | `crates/pms-server/src/api_fn/transaction.rs` | Intégration gas pool dans `wallet_send_tx()` |
| `pms-server` | `crates/pms-server/src/api_fn/wallet_factory.rs` | Intégration gas pool dans `wallet_send_simple()` |
| `pms-server` | `crates/pms-server/src/api_fn/nft.rs` | Intégration gas pool + storage fee dans `mint_nft()`, `burn_nft()`, etc. |
| `pms-server` | `crates/pms-server/src/api_fn/blocks.rs` | Intégration gas pool dans `submit_block()` |
| `pms-server` | `crates/pms-server/src/api_fn/bridge.rs` | Intégration cross-ledger fee dans `admin_bridge_transfer()` |
| `pms-server` | `crates/pms-server/src/api_fn/contracts.rs` | Intégration contract deployment fee dans `register_contract()` |
| `pms-server` | `crates/pms-server/src/api_fn/supply.rs` | `total_burned` exposé dans la réponse de `GET /v1/supply` |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `calculate_fee_burn(total_fees, burn_rate_bps)` | `crates/pms-economics/src/fee_burn.rs` | Calcule la portion brûlée vs distribuable. Retourne `FeeBurnResult { burned, distributable }`. Précision 8 décimales, cap à 10 000 bps. |
| `can_consume(pool, gas_per_tx, min_balance)` | `crates/pms-economics/src/gas_pool.rs` | Vérifie si le pool a assez de gas pour une transaction (sans modifier l'état). |
| `consume(pool, gas_per_tx, min_balance)` | `crates/pms-economics/src/gas_pool.rs` | Déduit le gas du pool. Retourne `Err(GasPoolError::InsufficientGas)` si insuffisant. Met à jour `balance` et `total_consumed`. |
| `deposit(pool, amount)` | `crates/pms-economics/src/gas_pool.rs` | Ajoute du PMS au pool. Rejette les montants <= 0. Met à jour `balance` et `total_deposited`. |
| `withdraw(pool, amount)` | `crates/pms-economics/src/gas_pool.rs` | Retire du PMS du pool. Rejette si `amount > balance`. |
| `TpsTracker::new(window_secs)` | `crates/pms-economics/src/dynamic_fee.rs` | Crée un tracker avec fenêtre glissante (défaut 60s). |
| `TpsTracker::record_block()` | `crates/pms-economics/src/dynamic_fee.rs` | Enregistre un timestamp de bloc (appelé après chaque `persist_block` réussi dans `persist_and_broadcast()`). |
| `TpsTracker::current_tps()` | `crates/pms-economics/src/dynamic_fee.rs` | Calcule le TPS courant (blocs dans la fenêtre / durée fenêtre). Prune automatiquement les entrées obsolètes. |
| `TpsTracker::fee_multiplier(target_tps, max_multiplier)` | `crates/pms-economics/src/dynamic_fee.rs` | Retourne `max(1.0, current_tps / target_tps)` plafonné à `max_multiplier`. |
| `TpsTracker::info(target_tps, max_multiplier)` | `crates/pms-economics/src/dynamic_fee.rs` | Retourne `DynamicFeeInfo { current_tps, target_tps, multiplier }` pour diagnostics/API. |
| `calculate_storage_fee(payload_bytes, fee_per_kb)` | `crates/pms-economics/src/storage_fee.rs` | Calcule `ceil(bytes / 1024) * fee_per_kb`. Minimum 1 KB. |
| `try_consume_gas(state)` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Wrapper d'intégration : consomme du gas pour les ledgers custom (no-op pour "main"). Appelé dans tous les tx handlers. |
| `dynamic_fee_multiplier(store, tps_tracker)` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Charge la RuntimeConfig, retourne le multiplicateur Decimal (1.0 si désactivé). |
| `load_storage_fee_per_kb(store, eff)` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Charge le fee per KB (priorité RuntimeConfig > EffectiveFees). |
| `load_burn_rate_bps(store, eff)` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Charge le taux de burn (priorité RuntimeConfig > EffectiveFees). |
| `load_contract_deployment_fee(store, eff)` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Charge le fee de déploiement de contrat. |
| `resolve_effective_fees(global, ledger_override)` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Fusionne la config globale avec les overrides per-ledger. |
| `perform_fee_distribution(state, parent_id)` | `crates/pms-server/src/fee_distribution.rs` | Distribution automatique des fees : calcule le burn, distribue le reste aux nodes/treasury, crée un bloc Mint. Appelle `calculate_fee_burn()` et `increment_total_burned()`. |
| `persist_and_broadcast(state, wb)` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Persiste un bloc et appelle `tps_tracker.record_block()` pour alimenter le calcul de TPS. |
| `increment_total_burned(amount)` | `crates/pms-storage/src/rocks_store/node_rewards_storage.rs` | Incrémente atomiquement le cumul brûlé dans RocksDB (CF `node_fee_pool`, clé `total_burned`). |
| `get_total_burned()` | `crates/pms-storage/src/rocks_store/node_rewards_storage.rs` | Lit le cumul brûlé depuis RocksDB. |

## Endpoints API

| Méthode | Path | Description |
|---------|------|-------------|
| `POST` | `/admin/gas-pool/deposit` | Déposer du PMS dans le gas pool d'un ledger. Body: `{ "ledger_id": "...", "amount": "50.0" }`. Requiert admin auth. Retourne `{ "status": "ok", "new_balance": "..." }`. |
| `POST` | `/admin/gas-pool/withdraw` | Retirer du PMS du gas pool d'un ledger. Body: `{ "ledger_id": "...", "amount": "10.0" }`. Requiert admin auth. Retourne 402 si solde insuffisant. |
| `GET` | `/v1/gas-pool/{ledger_id}` | Consulter le gas pool d'un ledger (public). Retourne `{ "ledger_id", "balance", "total_consumed", "total_deposited", "created_at" }`. Retourne 404 si pas de pool. |
| `GET` | `/v1/supply` | Supply en circulation. Inclut le champ `total_burned` (cumul des fees brûlées). |
| `POST` | `/admin/config` | Hot-swap de la RuntimeConfig. Supporte `SetBurnRate`, `SetContractDeploymentFee`, `SetStorageFeePerKb`, `SetDynamicFee`, et `BatchUpdate`. |
| `GET` | `/admin/config` | Lire la RuntimeConfig courante (inclut tous les champs Economics). |
| `POST` | `/admin/contracts` | Enregistrer un [[smart-contracts|contrat]]. Facture `contract_deployment_fee` si configuré. Retourne `{ "fee_charged": "10.0", "fee_block_id": "..." }`. |
| `POST` | `/admin/bridge/transfer` | Transfert inter-ledgers. Applique `cross_ledger_fee_multiplier` sur les frais standard. |

## RocksDB Column Families

| Column Family | Clé | Valeur | Usage |
|---------------|-----|--------|-------|
| `gas_pools` | `ledger_id` (bytes) | `GasPool` (JSON) | Stockage des gas pools per-ledger. Opérations atomiques read-modify-write. |
| `ledger_subscriptions` | `ledger_id` (bytes) | `LedgerSubscription` (JSON) | Stockage des abonnements annuels per-ledger (réservé, pas encore implémenté dans les handlers). |
| `node_fee_pool` | `"total_burned"` (bytes) | Decimal string (ex: `"1523.45"`) | Cumul des fees brûlées. Incrémenté atomiquement à chaque distribution. |

**Note** : Les deux CFs `gas_pools` et `ledger_subscriptions` sont déclarés dans :
- `RocksStore::CF_NAMES` (constante statique pour `open_db_multi_prefix()`)
- Le tableau inline dans `RocksStore::new()` (pour les ouvertures single-prefix)

Les deux listes **doivent** rester synchronisées. Historiquement, une désynchronisation entre ces deux listes a causé un crash au bootstrap (`ensure_column_families`).

## Types et Structures

### `GasPool` (`pms-types-economics`)

```rust
pub struct GasPool {
    pub ledger_id: String,
    pub balance: Decimal,         // Solde courant
    pub total_consumed: Decimal,  // Cumul à vie
    pub total_deposited: Decimal, // Cumul à vie
    pub created_at: i64,          // Unix timestamp (ms)
}
```

### `FeeBurnResult` (`pms-types-economics`)

```rust
pub struct FeeBurnResult {
    pub burned: Decimal,        // Montant retiré de la circulation
    pub distributable: Decimal, // Montant restant pour distribution
}
```

### `DynamicFeeInfo` (`pms-types-economics`)

```rust
pub struct DynamicFeeInfo {
    pub current_tps: f64,    // TPS mesuré
    pub target_tps: u32,     // Seuil cible
    pub multiplier: f64,     // Multiplicateur résultant (>= 1.0)
}
```

### `TpsTracker` (`pms-economics::dynamic_fee`)

```rust
pub struct TpsTracker {
    timestamps: Mutex<VecDeque<Instant>>, // Rolling window
    window_secs: u64,                     // Durée de la fenêtre (défaut 60s)
}
```

Thread-safe (Mutex interne). Capacité initiale de 4096 entrées. Les entrées obsolètes sont prunées automatiquement à chaque appel (`record_block`, `current_tps`, `fee_multiplier`).

### `GasPoolError` (`pms-economics::gas_pool`)

```rust
pub enum GasPoolError {
    InsufficientGas { balance: Decimal, required: Decimal, min_balance: Decimal },
    InsufficientBalance { balance: Decimal, requested: Decimal },
    InvalidAmount(Decimal),
}
```

### `EffectiveFees` (`pms-server::api_fn::tx_helpers`)

Structure résolue qui fusionne la config globale avec les overrides per-ledger. Contient tous les champs economics :

```rust
pub struct EffectiveFees {
    // ... (champs pré-existants) ...
    pub burn_rate_bps: u32,
    pub gas_per_tx: Option<String>,
    pub gas_pool_min_balance: Option<String>,
    pub contract_deployment_fee: Option<String>,
    pub storage_fee_per_kb: Option<String>,
}
```

### `RuntimeConfig` economics fields (`pms-config::runtime`)

```rust
// Dans RuntimeConfig :
pub burn_rate_bps: u32,                       // default: 0
pub contract_deployment_fee: Option<String>,  // default: None
pub storage_fee_per_kb: Option<String>,       // default: None
pub dynamic_fee_enabled: bool,                // default: false
pub target_tps: u32,                          // default: 100
pub max_fee_multiplier: f64,                  // default: 5.0
```

### `ConfigUpdate` economics variants (`pms-config::runtime`)

```rust
pub enum ConfigUpdate {
    // ... (variantes pré-existantes) ...
    SetBurnRate { bps: u32 },
    SetContractDeploymentFee { fee: Option<String> },
    SetStorageFeePerKb { fee: Option<String> },
    SetDynamicFee { enabled: bool, target_tps: Option<u32>, max_multiplier: Option<String> },
}
```

## Interactions

### 1. [[fee-distribution|Fee Distribution]] et Fee Burn

Le burn est appliqué dans `perform_fee_distribution()` (fichier `fee_distribution.rs`) :

1. Le pool de fees accumule les frais au fil des transactions
2. Toutes les `distribution_interval_sec` secondes (défaut 600s), le service automatique déclenche la distribution
3. `load_burn_rate_bps()` charge le taux de burn (RuntimeConfig > EffectiveFees)
4. `calculate_fee_burn(total_fees, burn_rate_bps)` sépare le total en `burned` + `distributable`
5. `increment_total_burned(burned)` persiste le montant brûlé dans RocksDB
6. Le `distributable` est distribué normalement (treasury tax, node rewards)

### 2. Config Hot-Swap

La chaîne de priorité pour tous les paramètres Economics :

```
RuntimeConfig (hot-swap, RocksDB) > EffectiveFees (per-ledger) > FeesSettings (config.toml)
```

Chaque fonction `load_*` dans `tx_helpers.rs` suit cette priorité :
- Charge `RuntimeConfig` depuis RocksDB (`get_runtime_config()`)
- Si la valeur est non-nulle dans RuntimeConfig, l'utilise
- Sinon, fallback sur `EffectiveFees` (qui est déjà la fusion de global + ledger override)

### 3. Création de Ledger et Gas Pool

Lors de la création d'un nouveau [[multi-ledger|ledger]] (`POST /admin/ledgers/create`) :
1. Le ledger est enregistré dans le `LedgerManager`
2. Un `GasPool` est auto-créé avec `balance: 0` via `put_gas_pool()`
3. L'administrateur doit ensuite approvisionner le pool via `POST /admin/gas-pool/deposit`
4. Sans gas, toutes les transactions sur ce ledger sont rejetées avec `402 Payment Required`

### 4. Transaction Handlers

Tous les handlers de transaction (6 points d'entrée) appellent `try_consume_gas()` en début de traitement :

| Handler | Fichier | Commentaire |
|---------|---------|-------------|
| `wallet_send_tx()` | `transaction.rs` | Transferts signés par le client |
| `wallet_send_simple()` | `wallet_factory.rs` | Transferts simplifiés (clé privée en body) |
| `submit_block()` | `blocks.rs` | Soumission de bloc générique |
| `mint_nft()` | `nft.rs` | Mint de NFT (+ storage fee sur metadata) |
| `burn_nft()` | `nft.rs` | Burn de NFT |
| `burn_nft_simple()` | `nft.rs` | Burn de NFT simplifié |
| `burn_nft_batch_simple()` | `nft.rs` | Burn de NFT par lot |

### 5. TPS Tracking

Le `TpsTracker` est alimenté par `persist_and_broadcast()` dans `tx_helpers.rs`. Chaque fois qu'un bloc est persisté avec succès (`PutResult::Inserted`), `tps_tracker.record_block()` est appelé. Le tracker est ensuite consulté par `dynamic_fee_multiplier()` pour calculer le multiplicateur de congestion.

L'instance est partagée dans `AppState.tps_tracker` (Arc) et initialisée avec une fenêtre de 60 secondes dans `serve_api()`.

### 6. Storage Fee sur NFT Mint

Dans `mint_nft()` :
1. La metadata NFT est sérialisée en JSON
2. La taille en bytes est mesurée
3. `load_storage_fee_per_kb()` charge le taux per-KB
4. `calculate_storage_fee(metadata_bytes, fee_per_kb)` calcule le surcharge
5. Le storage fee est additionné au NFT mint fee de base

### 7. Cross-Ledger Fee sur [[bridge|Bridge Transfer]]

Dans `admin_bridge_transfer()` :
1. Le transfert inter-ledgers est exécuté via le bridge engine
2. En cas de succès, `cross_ledger_fee_multiplier` est lu depuis `FeesSettings`
3. Le fee standard est calculé via `load_fee_policy()` + `compute_fee()`
4. Le fee cross-ledger = `base_fee * multiplier` (arrondi 8 décimales)
5. Un bloc Reward est créé pour distribuer ce fee supplémentaire
