---
tags: [feature]
created: 2026-01-10
updated: 2026-03-14
version: v0.3.0
---

# Node Rewards & Distribution

## Resume

Le systeme Node Rewards gere la remuneration des noeuds du reseau PMS en fonction de leur contribution a la production de blocs. Chaque bloc persiste dans le DAG incremente un compteur (`node_block_counts`) associe a la cle publique du signataire. Les frais de transaction sont accumules dans un pool persistant (`node_fee_pool`) en RocksDB. La distribution s'effectue selon deux mecanismes complementaires :

1. **Distribution via Milestone** (couche Core) : Lorsqu'un bloc `PlainPayload::Milestone { distribute_node_rewards: true }` est persiste, le `CoreAdapter` lit le pool et les compteurs depuis RocksDB, calcule la part proportionnelle de chaque mineur (en satoshis), cree des UTXOs de recompense dans le `ShardedUtxoSet`, puis reinitialise le pool et les compteurs. Chaque distribution emet un evenement `PmsEvent::NodeRewardDistributed`.

2. **Distribution automatique periodique** (couche Server) : Un timer asynchrone (`spawn_fee_distributor_task`) execute `perform_fee_distribution()` a intervalles reguliers (defaut : 600 secondes). Ce mecanisme opere sur le `FeePool` en memoire (RAM), avec un split configurable entre Coordinator, Treasury et noeuds, incluant fee burn et burn refunds. Le resultat est un bloc `PlainPayload::Mint` signe par le Coordinator.

Les deux systemes coexistent : le premier est le mecanisme historique de distribution bas-niveau (satoshis, RocksDB-only), le second est le systeme de production haut-niveau (Decimal, RAM FeePool, multi-beneficiaires, fee burn).

## Mecanisme

### 1. Accumulation des compteurs de blocs

A chaque bloc valide persiste dans le DAG (dans `CoreAdapter::persist_block()`), le compteur du signataire est incremente :

```rust
// crates/pms-core/src/net_adapter.rs, apres la persistance du bloc (etape 8)
if !wb.signer_pk_hex.trim().is_empty() {
    self.store.increment_node_block_count(&wb.signer_pk_hex)?;
}
```

Le compteur est stocke dans le CF RocksDB `node_block_counts` en format `u64` little-endian. Les lectures/ecritures sont single-writer (Coordinator-only), eliminant les race conditions.

### 2. Accumulation des fees (RocksDB)

Lors du traitement des transactions avec fees dans `CoreAdapter::persist_block()`, la portion treasury des fees est ajoutee au pool RocksDB :

```rust
// crates/pms-core/src/net_adapter.rs, dans le traitement TxUtxo
let treasury_portion = (fee_sats * treasury_fee_bps / 10000) as u64;
self.store.add_to_fee_pool(treasury_portion)?;
```

Le pool est stocke sous la cle `"pool"` dans le CF `node_fee_pool`.

### 3. Accumulation des fees (RAM FeePool)

En parallele, dans la couche Server (`api_fn/blocks.rs`), chaque fee de transaction est ajoutee au `FeePool` en memoire :

```rust
// crates/pms-server/src/api_fn/blocks.rs
pool.add_fee(fee, &signer_pk);
```

Le `FeePool` suit :
- `total_fees` : somme des fees accumulees (`Decimal`)
- `node_contributions` : `HashMap<String, u64>` -- nombre de blocs par cle publique
- `tx_count` : nombre total de transactions traitees
- `burn_refunds` : `HashMap<String, Decimal>` -- remboursements de burn en attente

### 4. Distribution via Milestone (Core)

Lorsqu'un bloc Milestone avec `distribute_node_rewards: true` est persiste, la distribution s'execute dans le lock finality de `CoreAdapter::persist_block()` :

1. **Lecture du pool** : `self.store.get_fee_pool()` -- montant total en satoshis
2. **Lecture des mineurs** : `self.store.get_all_miners()` -- `Vec<(String, u64)>`
3. **Calcul proportionnel** : `share = pool * block_count / total_blocks`
4. **Resolution des adresses** : `self.store.get_node_reward_address(node_pk)` -- retourne l'adresse configuree ou la cle publique par defaut
5. **Creation des UTXOs** : Chaque part est convertie en `TxOutput` (montant en PMS = satoshis / 100_000_000)
6. **Reset** : `self.store.reset_pool_and_counts()` -- remet le pool a zero et supprime tous les compteurs via `WriteBatch`

Les UTXOs sont ajoutees au `ShardedUtxoSet` apres la liberation du lock finality, et un evenement `PmsEvent::NodeRewardDistributed` est emis pour chaque noeud remunere.

### 5. Distribution automatique periodique (Server)

Le timer `spawn_fee_distributor_task()` execute `perform_fee_distribution()` qui :

1. Verifie que le noeud est le Coordinator
2. Resout le parent via `top_tips(1)`
3. Lit le `FeePool` en memoire
4. Applique le fee burn (`burn_rate_bps`) via `calculate_fee_burn()`
5. Construit les outputs :
   - **Burn refunds** : directement vers les wallets utilisateurs
   - **Treasury tax** : `treasury_fee_percent` (defaut 35%) vers un wallet treasury
   - **Node rewards** : montant restant distribue proportionnellement aux noeuds selon `node_contributions`
6. Cree un bloc `PlainPayload::Mint` avec PoW, signe par le Coordinator
7. Persiste le bloc, cree les UTXOs, et broadcast
8. Remet le `FeePool` a zero

Le `NodeRegistry` est consulte pour resoudre les adresses de wallet des noeuds. Si un noeud n'a pas d'adresse enregistree, sa part est redirigee vers le Treasury (fallback de securite).

### 6. Adresses de recompense configurables

Chaque noeud peut definir une adresse de recompense distincte de sa cle publique via `set_node_reward_address()`. Si aucune adresse n'est configuree, la cle publique est utilisee par defaut. Cette fonctionnalite est utilisee dans le mecanisme Milestone (Core) mais pas dans la distribution periodique (Server), qui utilise le `NodeRegistry`.

### 7. Fee Burn (mecanisme deflationniste)

Avant distribution periodique, une fraction des fees est detruite si `burn_rate_bps > 0`. Le montant brule est persiste dans RocksDB (`node_fee_pool`, cle `"total_burned"`) sous forme de `Decimal` string, via `increment_total_burned()`.

### 8. Inflation programmee

`perform_daily_inflation_mint()` cree periodiquement des blocs de mint bases sur le supply en circulation : `daily_amount = circulating_supply * annual_inflation_percent / 365`. La distribution suit un split creator/treasury/burn configurable.

## Configuration

### Parametres TOML (`[fees]`)

| Parametre | Type | Defaut | Description |
|-----------|------|--------|-------------|
| `distribution_interval_sec` | `u64` | `600` (10 min) | Intervalle entre deux distributions automatiques. `0` desactive. |
| `coordinator_fee_percent` | `u8` | `65` | Pourcentage des fees allant au Coordinator. |
| `treasury_fee_percent` | `u8` | `35` | Pourcentage des fees allant au Treasury. |
| `treasury_addresses` | `Vec<String>` | `[]` | Adresses Bech32m des wallets treasury. |
| `burn_rate_bps` | `u32` | `0` | Taux de burn des fees en basis points (3000 = 30%). |
| `annual_inflation_percent` | `f64` | `3.0` | Taux d'inflation annuel. |
| `creator_reward_percent` | `u8` | `70` | Part de l'inflation vers le Coordinator. |
| `treasury_reward_percent` | `u8` | `20` | Part de l'inflation vers le Treasury. |
| `burn_percent` | `u8` | `10` | Part de l'inflation brulee. |
| `daily_inflation_enabled` | `bool` | `false` | Active l'inflation programmee. |
| `daily_inflation_interval_sec` | `u64` | `86400` (24h) | Intervalle du mint d'inflation. |
| `block_reward` | `String` | `"0.1"` | Recompense par bloc (PMS). |

### RuntimeConfig (hot-swap via blocs `ConfigUpdate`)

| Parametre | Defaut | Description |
|-----------|--------|-------------|
| `coordinator_fee_bps` | `6700` (67%) | Part du Coordinator en basis points. |
| `treasury_fee_bps` | `3300` (33%) | Part du Treasury en basis points. |
| `fee_rate_bps` | `300` (3%) | Taux de commission global. |

La somme `coordinator_fee_bps + treasury_fee_bps` doit etre exactement 10000 (100%). Validee par `RuntimeConfig::validate_fee_split()`.

### ConfigUpdate applicables

| Variant | Description |
|---------|-------------|
| `SetCoordinatorFee { bps }` | Modifier la part du Coordinator |
| `SetTreasuryFee { bps }` | Modifier la part du Treasury |
| `SetFeeRate { bps }` | Modifier le taux de commission |
| `SetBurnRate { bps }` | Modifier le taux de burn |
| `BatchUpdate(updates)` | Appliquer plusieurs modifications atomiquement |

### BlockRewardConfig

Structure de configuration pour les recompenses de bloc (inflation) dans `fee_distribution.rs` :

| Champ | Type | Defaut | Description |
|-------|------|--------|-------------|
| `reward_per_block` | `String` | `"0.1"` | Recompense par bloc en PMS |
| `creator_percent` | `u8` | `70` | Pourcentage vers le createur du bloc |
| `treasury_percent` | `u8` | `20` | Pourcentage vers le treasury |
| `burn_percent` | `u8` | `10` | Pourcentage brule |

## RocksDB Column Families

| CF | Cles | Format Valeur | Description | Fichier d'implementation |
|----|------|---------------|-------------|--------------------------|
| `node_block_counts` | `<node_pk>` (bytes UTF-8) | `u64` (8 bytes little-endian) | Compteur de blocs mines par noeud | `node_rewards_storage.rs` |
| `node_fee_pool` | `"pool"` | `u64` (8 bytes little-endian) | Montant total du pool de fees persistant (satoshis) | `node_rewards_storage.rs` |
| `node_fee_pool` | `"total_burned"` | `Decimal` (string UTF-8) | Cumul des fees brulees (deflationniste) | `node_rewards_storage.rs` |
| `node_reward_addresses` | `<node_pk>` (bytes UTF-8) | `address` (bytes UTF-8) | Adresse de recompense par noeud (optionnel, defaut = pk) | `node_rewards_storage.rs` |

Les trois CFs sont declares dans les deux listes de `store.rs` (`CF_NAMES` et le tableau hardcode dans `new()`) et doivent toujours rester synchronises.

## Crates et Fichiers

| Crate | Fichier | Role |
|-------|---------|------|
| `pms-storage` | `src/node_rewards.rs` | Trait `NodeRewardsStorage` -- interface abstraite pour le stockage des recompenses |
| `pms-storage` | `src/rocks_store/node_rewards_storage.rs` | Implementation RocksDB de `NodeRewardsStorage` + `increment_total_burned()` / `get_total_burned()` |
| `pms-storage` | `src/rocks_store/store.rs` | Declaration des CFs `node_block_counts`, `node_fee_pool`, `node_reward_addresses` |
| `pms-core` | `src/net_adapter.rs` | `persist_block()` -- increment compteur, accumulation fee pool, distribution Milestone |
| `pms-server` | `src/fee_pool.rs` | Structure `FeePool` -- accumulation RAM des fees, calcul des parts, burn refunds |
| `pms-server` | `src/fee_distribution.rs` | `perform_fee_distribution()`, `perform_daily_inflation_mint()`, `compute_fee_outputs()`, `compute_block_reward_outputs()`, `BlockRewardConfig` |
| `pms-server` | `src/api_fn/milestone.rs` | Endpoints `POST /admin/distribute_fees`, `GET /v1/fee_pool` |
| `pms-server` | `src/api_fn/blocks.rs` | Accumulation des fees dans le `FeePool` lors de la validation des blocs |
| `pms-server` | `src/api.rs` | `spawn_fee_distributor_task()`, `spawn_inflation_mint_task()` |
| `pms-server` | `src/node_registry.rs` | `NodeRegistry`, `NodeInfo` -- registre des noeuds avec `wallet_address` pour la distribution |
| `pms-config` | `src/config.rs` | `FeesSettings` -- parametres TOML de fees et distribution |
| `pms-config` | `src/runtime.rs` | `RuntimeConfig`, `ConfigUpdate` -- hot-swap des parametres de fees |
| `pms-event` | `src/events.rs` | `PmsEvent::NodeRewardDistributed` -- evenement emis a chaque distribution |
| `pms-types-payload` | `src/payload.rs` | `PlainPayload::Milestone`, `PlainPayload::Reward`, `PlainPayload::EncryptedReward`, `PlainPayload::Mint` |
| `pms-economics` | `src/fee_burn.rs` | `calculate_fee_burn()` -- calcul de la fraction a bruler |
| `pms-testkit` | `src/coordinator.rs` | `TestCoordinator::forge_milestone()` -- helper de test pour creer des Milestones signes |

## Fonctions Cles

### Trait `NodeRewardsStorage` (`pms-storage/src/node_rewards.rs`)

| Fonction | Description |
|----------|-------------|
| `get_node_block_count(node_pk)` | Recupere le nombre de blocs mines par un noeud (u64, defaut 0) |
| `increment_node_block_count(node_pk)` | Incremente le compteur de blocs (read + write, single-writer) |
| `get_fee_pool()` | Recupere le montant total du pool de fees (u64 satoshis) |
| `add_to_fee_pool(amount)` | Ajoute un montant au pool de fees (read + add + write) |
| `get_all_miners()` | Recupere tous les mineurs et leurs compteurs via iterateur RocksDB |
| `reset_pool_and_counts()` | Reinitialise le pool a 0 et supprime tous les compteurs via `WriteBatch` |
| `set_node_reward_address(node_pk, address)` | Definit une adresse de recompense pour un noeud |
| `get_node_reward_address(node_pk)` | Recupere l'adresse de recompense (ou la cle publique par defaut) |

### Fonctions RocksStore additionnelles (`pms-storage/src/rocks_store/node_rewards_storage.rs`)

| Fonction | Description |
|----------|-------------|
| `increment_total_burned(amount)` | Incremente atomiquement le total cumule des fees brulees (Decimal) |
| `get_total_burned()` | Lit le total cumule des fees brulees |

### Distribution Core (`pms-core/src/net_adapter.rs`)

| Zone | Description |
|------|-------------|
| Etape 8 (`persist_block`) | `increment_node_block_count(&wb.signer_pk_hex)` -- comptage des blocs |
| Etape 6a (finality lock) | Distribution Milestone : lecture pool/mineurs, calcul proportionnel, creation UTXOs |
| Etape 6c (post-lock) | Ajout des UTXOs au `ShardedUtxoSet`, emission `PmsEvent::NodeRewardDistributed` |

### Distribution Server (`pms-server/src/fee_distribution.rs`)

| Fonction | Description |
|----------|-------------|
| `perform_fee_distribution(state, parent_id)` | Distribution complete : fee burn, treasury tax, node rewards, creation bloc Mint |
| `perform_daily_inflation_mint(state)` | Mint d'inflation quotidien base sur le supply en circulation |
| `compute_fee_outputs(total, treasury_addrs, coord_addr, config)` | Calcule les outputs N-way a partir de `FeeDistributionConfig` |
| `compute_block_reward_outputs(creator, treasury, config)` | Calcule les outputs de recompense de bloc (creator/treasury/burn) |

### FeePool (`pms-server/src/fee_pool.rs`)

| Fonction | Description |
|----------|-------------|
| `add_fee(fee, node_pk)` | Ajoute une fee au pool avec suivi de contribution |
| `add_burn_refund(wallet_address, amount)` | Ajoute un remboursement de burn |
| `calculate_shares()` | Calcule les parts proportionnelles : `Vec<(node_pk, share_pct, amount)>` |
| `has_fees()` | Retourne `true` si le pool a des fees ou refunds a distribuer |
| `reset()` | Remet tout a zero apres distribution |

## Endpoints API

| Methode | Path | Description | Acces |
|---------|------|-------------|-------|
| `GET` | `/v1/fee_pool` | Statut du pool : `total_fees`, `total_burn_refunds`, `tx_count`, `num_contributors` | Public |
| `POST` | `/admin/distribute_fees` | Declenche une distribution manuelle. Body optionnel : `{ "parent_id": "..." }` | Coordinator-only (403 sinon) |
| `GET` | `/v1/supply` | Supply circulant avec champ `total_burned` | Public |

### Format de reponse de `POST /admin/distribute_fees`

```json
{
  "success": true,
  "reward_block_id": "abc123...",
  "total_distributed": "42.50000000",
  "num_recipients": 3
}
```

### Format de reponse de `GET /v1/fee_pool`

```json
{
  "total_fees": "125.35000000",
  "total_burn_refunds": "2.50000000",
  "burn_refund_count": 1,
  "tx_count": 47,
  "num_contributors": 2
}
```

## Evenements

L'evenement `PmsEvent::NodeRewardDistributed` est emis par `CoreAdapter` (couche Core) lors de la distribution via Milestone :

```rust
PmsEvent::NodeRewardDistributed {
    node_pk: String,        // Cle publique du noeud
    address: String,        // Adresse de recompense
    amount_sats: u64,       // Montant en satoshis
    milestone_id: String,   // ID du Milestone declencheur
}
```

Type d'evenement string : `"node_reward_distributed"` (via `PmsEvent::event_type()`).

## Interactions

### Avec [[fee-distribution]]

Le systeme Node Rewards et le systeme Fee Distribution partagent les memes CFs RocksDB (`node_fee_pool`, `node_block_counts`, `node_reward_addresses`) et le meme `FeePool` en memoire. La distribution periodique dans `perform_fee_distribution()` est le mecanisme principal de production qui distribue les fees aux noeuds via des blocs `Mint`. Le mecanisme Milestone dans `CoreAdapter` est le mecanisme historique de bas-niveau. Les deux mecanismes reinitialisant les compteurs apres distribution, ils operent sur des pools distincts (RocksDB vs RAM).

### Avec [[event-system]]

Chaque distribution via Milestone emet un `PmsEvent::NodeRewardDistributed` via le `EventBus`. Ces evenements peuvent etre consommes par les clients SSE (Server-Sent Events) pour des notifications en temps reel de recompenses.

### Avec [[milestones-finality]]

Le Milestone est le declencheur de la distribution dans la couche Core. Le payload `PlainPayload::Milestone { approved, distribute_node_rewards }` controle si les fees doivent etre distribuees. Le Coordinator forge le Milestone via `TestCoordinator::forge_milestone()` (en test) ou via l'emission programmatique d'un bloc Milestone en production. Le Milestone marque egalement les blocs comme finalises, ce qui est orthogonal mais lie au meme bloc.

### Avec [[economics]]

Le systeme Economics fournit le fee burn (`calculate_fee_burn()`), le gas pool (verification `try_consume_gas()` avant distribution), et les subscriptions (`check_subscription_active()` pour les ledgers custom). Le `burn_rate_bps` est applique dans `perform_fee_distribution()` avant la distribution des shares.

### Avec [[storage-rocksdb]]

Les trois CFs de recompenses (`node_block_counts`, `node_fee_pool`, `node_reward_addresses`) sont declares aux positions 19-21 dans la liste des CFs de `RocksStore`. Le `reset_pool_and_counts()` utilise un `WriteBatch` atomique pour supprimer tous les compteurs de mineurs en une seule ecriture.

### Avec [[config-system]]

Les parametres de distribution sont configurables a trois niveaux :
1. **TOML** (`[fees]`) : configuration statique au demarrage
2. **Per-ledger overrides** (`LedgerFeesOverride`) : surcharges par ledger dans les systemes multi-ledger
3. **RuntimeConfig** (RocksDB) : hot-swap via blocs `ConfigUpdate` signes par le Coordinator

La chaine de priorite est : RuntimeConfig > EffectiveFees (per-ledger) > FeesSettings (TOML global).

## Tests

| Fichier | Description |
|---------|-------------|
| `crates/pms-storage/tests/node_rewards_test.rs` | Tests unitaires du trait `NodeRewardsStorage` : compteurs, pool, get_all_miners, reset, RuntimeConfig treasury |
| `crates/pms-core/tests/node_rewards_wallets.rs` | Test E2E complet : 3 noeuds minent des blocs, fees accumulees, Milestone distribue, UTXOs crees, balances proportionnelles |
| `crates/pms-server/tests/automated_distribution_test.rs` | Test de la distribution automatique avec MockAdapter, timer 1s, verification que le pool est draine |
| `crates/pms-server/src/fee_distribution.rs` (mod tests) | Tests unitaires : validation config, N-way split, `BlockRewardConfig`, `compute_block_reward_outputs()` |
| `crates/pms-server/src/fee_pool.rs` (mod tests) | Tests unitaires : shares proportionnelles (75%/25%), precision Decimal (66.66666667 / 33.33333333) |

## Securite

- **Coordinator-only** : Seul le Coordinator peut emettre des Milestones et declencher des distributions. La cle publique est verifiee dans `persist_block()` et `perform_fee_distribution()`.
- **Single-writer** : Les operations sur `node_block_counts` et `node_fee_pool` sont serialisees par le Coordinator, eliminant les race conditions sur les compteurs.
- **Precision financiere** : Les calculs dans la couche Server utilisent `rust_decimal::Decimal` avec arrondi a 8 decimales. La couche Core utilise des `u64` en satoshis pour la precision entiere.
- **Reset atomique** : `reset_pool_and_counts()` utilise un `WriteBatch` RocksDB pour supprimer tous les compteurs en une seule operation atomique.
- **Fallback de securite** : Si un noeud n'a pas de wallet enregistre dans le `NodeRegistry`, sa part est redirigee vers le Treasury. Si aucun Treasury n'est configure, les fonds restent dans le pool (aucune perte de fonds).
- **Protection anti-tipless** : `perform_fee_distribution()` refuse de distribuer si `top_tips()` retourne vide, evitant la creation de blocs orphelins ou la perte de fonds.
- **Validation bps** : La somme `coordinator_fee_bps + treasury_fee_bps` est validee a exactement 10000 par `RuntimeConfig::validate_fee_split()`.
