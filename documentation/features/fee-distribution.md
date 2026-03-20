---
tags: [feature]
created: 2026-01-10
updated: 2026-03-20
version: v0.5.19
---

# Fee Distribution (Distribution Automatique des Frais)

## Résumé

Le système de Fee Distribution gère la collecte, l'accumulation et la distribution périodique des frais de transaction dans le réseau PMS. Les frais sont collectés à chaque transaction dans un pool en mémoire (`FeePool`), puis distribués automatiquement à intervalles réguliers via un bloc `Mint` signé par le Coordinator. La distribution répartit les frais entre le Coordinator (65% par défaut), le Treasury (35% par défaut) et les nœuds participants, avec possibilité de N-way split configurable. Le système intègre également un mécanisme de [[economics|fee burn]] (destruction permanente d'une fraction des frais), des burn refunds (remboursements de [[smart-contracts|contrats smart]]), et une inflation programmée quotidienne.

Ce mécanisme est essentiel au modèle économique du réseau : il rémunère les opérateurs de nœuds, alimente la trésorerie du projet, et contrôle la masse monétaire via le burn déflationniste.

### Transfer Fees (v0.5.5)

En plus des frais de gas PMS et des burn refunds, le système supporte désormais les **frais de transfert smart contract** : un pourcentage ou montant fixe prélevé au sender lors de chaque transfert, routé vers un bénéficiaire fixe (ex: le créateur du ledger). Ces frais sont des `TxOutput` additionnels dans la transaction elle-même — ils ne passent PAS par le FeePool ni la fee distribution. Voir [[smart-contracts#Transfer Fees → TX Preparation (v0.5.5)]] pour les détails.

## Dates

| | Date |
|---|---|
| Créée | 2026-01-10 (commit `1f3d2ba`) |
| Dernière mise à jour | 2026-03-15 |
| Version d'introduction | v0.1.0 |

### Historique des changements majeurs

| Date | Commit | Description |
|------|--------|-------------|
| 2026-01-10 | `1f3d2ba` | `feat: Encrypted Reward Blocks + Treasury Fee Distribution` -- Création initiale avec blocs EncryptedReward, distribution coordinator/treasury, e2e test |
| 2026-02-16 | `a71d609` | `fix(fees): reinforce fee system with bps precision, validation, and enforcement` -- Passage en basis points, validation, N-way split |
| 2026-03-01 | `9e2922f` | `fix(dag+fees): prevent tipless DAG and silent fee distribution failure` -- Protection du dernier tip RAM pour éviter blocage de distribution |
| 2026-03-11 | `89e1a2f` | `fix(storage): protect last RocksDB tip from deletion -- unblock fee distribution` -- Protection duale RAM+RocksDB, diagnostics améliorés |
| 2026-03-13 | `1bb09bd` | `fix(perf): resolve TPS degradation + add smart contract system (v0.2.1)` -- Intégration fee burn et contrats smart |
| 2026-03-14 | branche `feature/economics` | Intégration fee burn (`burn_rate_bps`), gas pool, subscriptions, dynamic fees |
| 2026-03-15 | v0.5.1 | Fix: `contract_store` field in `AppState` — burn refunds now work on custom ledgers (contracts looked up from main store) |
| 2026-03-18 | v0.5.12 | Fix: PMS fee bootstrap deadlock on custom ledgers — protocol fee waived when PMS unavailable for custom asset transfers |
| 2026-03-18 | v0.5.15 | Fix: Supply double-counting — removed redundant `add_utxo` calls after `persist_block` for Mint and Reward blocks. `persist_block` already handles UtxoDelta → `apply_diff()` for plain payloads. |
| 2026-03-19 | v0.5.16 | Test: DAG Sandbox (`dag_sandbox.rs`) — integration test verifying coordinator receives fees from eden transactions via immediate Reward blocks. |
| 2026-03-19 | v0.5.18 | Fix: Supply endpoint wallet balances (`admin_balance`, `node_balance`, `treasury_balance`) now use `balance_by_address_and_asset()` — correctly displays EDN (or other custom token) balances instead of always PMS native. |

## Mécanisme

### 1. Collecte des frais

Les frais sont collectés à chaque transaction validée par le Coordinator. Lors de la validation d'un bloc contenant une transaction (`TxUtxo`), le montant du fee est ajouté au `FeePool` en mémoire via `pool.add_fee(fee, &signer_pk)`. Chaque appel enregistre :
- Le montant du fee dans `total_fees`
- La contribution du nœud signataire dans `node_contributions` (nombre de blocs créés)
- Un compteur de transactions `tx_count`

Les sources de fees incluent :
- **Frais de transaction standard** (`TxUtxo`) -- accumulés dans `blocks.rs`
- **Frais de mint de token** -- accumulés dans `token.rs`
- **Frais de mint de [[nft-system|NFT]]** -- accumulés dans `nft.rs`
- **Burn refunds** ([[smart-contracts|contrats smart]]) -- via `pool.add_burn_refund(address, amount)` dans `nft.rs`

### 2. Distribution périodique

Un timer asynchrone (`spawn_fee_distributor_task`) exécute `perform_fee_distribution()` à intervalles réguliers (défaut: 600 secondes / 10 minutes). Le cycle de distribution suit ces étapes :

1. **Vérification d'autorisation** : Seul le Coordinator (ou mode dev) peut distribuer.
2. **Résolution du parent** : Récupère le tip le plus récent du DAG via `top_tips(1)`. Si le DAG est vide (over-pruned), la distribution est bloquée avec un warning.
3. **Lecture du pool** : Récupère `total_fees`, `shares` (parts par nœud), et `burn_refunds`.
4. **Fee burn** : Calcule et soustrait la fraction à brûler (`burn_rate_bps`) via `calculate_fee_burn()`. Le montant brûlé est persisté dans RocksDB (`increment_total_burned()`).
5. **Construction des outputs** :
   - **Burn refunds** : Outputs directs vers les wallets utilisateurs (remboursements de contrats)
   - **Treasury tax** : Pourcentage (`treasury_fee_percent`) prélevé vers un wallet treasury
   - **Node rewards** : Montant restant distribué proportionnellement aux nœuds selon leurs contributions (nombre de blocs)
6. **Création du bloc Mint** : Un bloc `PlainPayload::Mint` est créé avec tous les outputs, miné (PoW si requis), signé par le Coordinator.
7. **Persistance et broadcast** : Le bloc est persisté dans le DAG, les UTXOs sont créés, et le bloc est broadcast aux pairs.
8. **Reset du pool** : Le `FeePool` est remis à zéro.

### 2b. Custom Ledger Fee Bootstrap (v0.5.12)

Sur les ledgers custom (ex: eden), le token principal est un custom asset (ex: EDN). Le token PMS natif n'existe pas initialement sur ces ledgers. Cela créait un deadlock :

1. Les transferts d'EDN nécessitent du PMS pour le protocol fee
2. Le PMS n'apparaît sur eden que via `create_reward_block` (après un transfert réussi)
3. Aucun transfert ne peut réussir sans PMS → deadlock

**Fix (v0.5.12)** : Quand un agent n'a pas de PMS pour le protocol fee lors d'un transfert de custom asset, le fee est gracieusement waivé (`fee_dec = 0`). Les smart contract transfer fees (en EDN) s'appliquent toujours et fournissent les revenus au créateur du ledger. Une fois que du PMS apparaît sur le ledger (ex: via bridge), le protocol fee reprend automatiquement.

### 3. Distribution unifiée via FeePool (v0.5.19)

**Depuis v0.5.19**, toutes les fees de transaction (wallet_send_simple, send_tx, token creation, NFT mint, contract deploy, bridge) sont accumulées dans le `FeePool` via `accumulate_tx_fee()`, puis distribuées périodiquement via `spawn_fee_distributor_task`. Cette unification remplace les anciens blocs `Reward` per-TX (`create_reward_block()`, désormais déprécié) qui causaient une prolifération d'UTXOs au coordinateur (7200 UTXOs/heure à 2000 TPS → dégradation des performances de coin selection).

**Atomic swap (v0.5.19)**: `perform_fee_distribution()` utilise `std::mem::replace` pour échanger atomiquement le pool avec un pool vide avant de distribuer. Cela élimine une race condition où les fees accumulées entre le snapshot et le reset étaient perdues. En cas d'échec de persistance, `FeePool::merge_from()` restaure les fees dans le pool.

### 4. Reward blocks chiffrés (EncryptedReward)

Pour la production, le système supporte des blocs `PlainPayload::EncryptedReward` où chaque output est chiffré individuellement pour son destinataire et le Coordinator (X25519). Ce mécanisme garantit la confidentialité des montants distribués tout en permettant l'auditabilité par le Coordinator.

### 5. Inflation programmée

Le système `perform_daily_inflation_mint()` crée périodiquement des blocs de mint basés sur le supply en circulation : `daily_amount = circulating_supply * annual_inflation_percent / 365`. La distribution suit un split configurable entre le Coordinator (`creator_reward_percent`, défaut 70%), le Treasury (`treasury_reward_percent`, défaut 20%), et le burn (`burn_percent`, défaut 10%).

### 6. Fee burn (mécanisme déflationniste)

Lorsque `burn_rate_bps > 0`, une fraction des fees est définitivement détruite avant distribution. Exemple : avec `burn_rate_bps = 3000` (30%), sur 100 PMS de fees, 30 PMS sont brûlés et 70 PMS sont distribués. Le total cumulatif est suivi dans RocksDB (`total_burned`) et exposé via `GET /v1/supply`.

## Configuration

### Paramètres TOML (`[fees]`)

| Paramètre | Type | Défaut | Description |
|-----------|------|--------|-------------|
| `distribution_interval_sec` | `u64` | `600` (10 min) | Intervalle entre deux distributions automatiques. `0` désactive. |
| `coordinator_fee_percent` | `u8` | `65` | Pourcentage des fees allant au Coordinator (mode simple). |
| `treasury_fee_percent` | `u8` | `35` | Pourcentage des fees allant au Treasury (mode simple). |
| `treasury_addresses` | `Vec<String>` | `[]` | Adresses Bech32m des wallets treasury. |
| `fee_distribution` | `Option<FeeDistributionConfig>` | `None` | Split N-way configurable (remplace coordinator/treasury si présent). |
| `burn_rate_bps` | `u32` | `0` | Taux de burn des fees en basis points (3000 = 30%). |
| `annual_inflation_percent` | `f64` | `3.0` | Taux d'inflation annuel en pourcentage. |
| `creator_reward_percent` | `u8` | `70` | Part de l'inflation vers le Coordinator. |
| `treasury_reward_percent` | `u8` | `20` | Part de l'inflation vers le Treasury. |
| `burn_percent` | `u8` | `10` | Part de l'inflation brûlée. |
| `daily_inflation_enabled` | `bool` | `false` | Active l'inflation programmée. |
| `daily_inflation_interval_sec` | `u64` | `86400` (24h) | Intervalle du mint d'inflation. |

### FeeDistributionConfig (N-way split)

Permet un split arbitraire entre N bénéficiaires. Les `percent_bps` doivent sommer à 10000 (100%).

```toml
[fees.fee_distribution]
beneficiaries = [
  { role = "coordinator", percent_bps = 5000 },
  { role = "client", percent_bps = 3000, address = "8e1abc..." },
  { role = "treasury", percent_bps = 2000 }
]
```

Rôles spéciaux (résolus automatiquement au runtime) :
- `"coordinator"` : utilise l'adresse du node wallet
- `"treasury"` : sélectionne aléatoirement parmi `treasury_addresses`
- Autre rôle : nécessite un champ `address` explicite

### Treasury Wallets (fichier signé)

Le fichier `treasury_wallets_file` (JSON) contient la liste des wallets treasury signée par le Coordinator :

```json
{
  "wallets": ["8e1abc...", "8e1def..."],
  "signature": "hex_encoded_coordinator_signature"
}
```

Message signé : `PMS_TREASURY_v1:<wallet1>,<wallet2>,...`

### RuntimeConfig (hot-swap)

Les paramètres suivants peuvent être modifiés à chaud via des blocs `ConfigUpdate` sans redémarrage :

| ConfigUpdate | Description |
|-------------|-------------|
| `SetCoordinatorFee { bps }` | Modifier la part du Coordinator (basis points) |
| `SetTreasuryFee { bps }` | Modifier la part du Treasury (basis points) |
| `SetFeeDistribution { beneficiaries }` | Définir un split N-way (valide que total = 10000) |
| `SetBurnRate { bps }` | Modifier le taux de burn (max 10000) |
| `SetFeeRate { bps }` | Modifier le taux de commission |
| `SetBaseFee { fee }` | Modifier les frais fixes |
| `BatchUpdate(updates)` | Appliquer plusieurs modifications atomiquement |

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-server` | `src/fee_distribution.rs` | Logique principale : `perform_fee_distribution()`, `perform_daily_inflation_mint()`, `compute_fee_outputs()`, `compute_block_reward_outputs()` |
| `pms-server` | `src/fee_pool.rs` | Structure `FeePool` : accumulation des fees, calcul des parts, burn refunds |
| `pms-server` | `src/api_fn/milestone.rs` | Endpoints : `POST /admin/distribute_fees`, `GET /v1/fee_pool` |
| `pms-server` | `src/api_fn/tx_helpers.rs` | `create_reward_block()`, `compute_fee_outputs()`, `EffectiveFees`, `load_burn_rate_bps()`, `resolve_effective_fees()` |
| `pms-server` | `src/api_fn/supply.rs` | `GET /v1/supply` : expose `total_burned` |
| `pms-server` | `src/api_fn/blocks.rs` | Accumulation des fees dans le pool lors de la validation des blocs |
| `pms-server` | `src/api_fn/token.rs` | Accumulation des fees de mint/création de token |
| `pms-server` | `src/api_fn/nft.rs` | Accumulation des fees de mint NFT et burn refunds (contrats) |
| `pms-server` | `src/api.rs` | `spawn_fee_distributor_task()`, `spawn_inflation_mint_task()`, `AppState.fee_pool` |
| `pms-config` | `src/config.rs` | `FeesSettings`, `FeeDistributionConfig`, `FeeBeneficiary`, `LedgerFeesOverride` |
| `pms-config` | `src/runtime.rs` | `RuntimeConfig`, `ConfigUpdate` (hot-swap des paramètres de fees) |
| `pms-config` | `src/treasury_wallets.rs` | `TreasuryWallets`, `TreasuryWalletsFile` : chargement et vérification des wallets treasury signés |
| `pms-economics` | `src/fee_burn.rs` | `calculate_fee_burn()` : calcul de la fraction à brûler |
| `pms-types-economics` | `src/lib.rs` | `FeeBurnResult` : structure retour du calcul de burn |
| `pms-types-payload` | `src/payload.rs` | `PlainPayload::Reward`, `PlainPayload::EncryptedReward`, `PlainPayload::Mint` : types de blocs de distribution |
| `pms-storage` | `src/node_rewards.rs` | Trait `NodeRewardsStorage` : interface RocksDB pour pool de fees et compteurs de blocs |
| `pms-storage` | `src/rocks_store/node_rewards_storage.rs` | Implémentation RocksDB : `increment_total_burned()`, `get_total_burned()`, compteurs de blocs par nœud |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `perform_fee_distribution()` | `pms-server/src/fee_distribution.rs` | Distribue les fees accumulées : burn, treasury tax, node rewards. Crée un bloc `Mint`. |
| `perform_daily_inflation_mint()` | `pms-server/src/fee_distribution.rs` | Mint quotidien d'inflation basé sur le supply en circulation. |
| `compute_fee_outputs()` | `pms-server/src/fee_distribution.rs` | Calcule les outputs de distribution N-way à partir du total et de la config. |
| `compute_block_reward_outputs()` | `pms-server/src/fee_distribution.rs` | Calcule les outputs de récompense de bloc (creator/treasury/burn). |
| `spawn_fee_distributor_task()` | `pms-server/src/api.rs` | Lance le timer asynchrone pour la distribution périodique. |
| `spawn_inflation_mint_task()` | `pms-server/src/api.rs` | Lance le timer asynchrone pour l'inflation programmée. |
| `accumulate_tx_fee()` | `pms-server/src/api_fn/tx_helpers.rs` | Accumule une fee dans le FeePool pour distribution consolidée (v0.5.19). Remplace `create_reward_block()`. |
| `create_reward_block()` | `pms-server/src/api_fn/tx_helpers.rs` | **DEPRECATED** — Créait un bloc `Reward` immédiat per-TX. Cause prolifération d'UTXOs. Remplacé par `accumulate_tx_fee()`. |
| `FeePool::add_fee()` | `pms-server/src/fee_pool.rs` | Ajoute une fee au pool avec suivi de contribution du nœud. |
| `FeePool::merge_from()` | `pms-server/src/fee_pool.rs` | Fusionne un snapshot de pool (récupération d'erreur après swap atomique, v0.5.19). |
| `FeePool::add_burn_refund()` | `pms-server/src/fee_pool.rs` | Ajoute un remboursement de burn pour un wallet utilisateur. |
| `FeePool::calculate_shares()` | `pms-server/src/fee_pool.rs` | Calcule les parts proportionnelles de chaque nœud (bloc count / total blocks). |
| `FeePool::reset()` | `pms-server/src/fee_pool.rs` | Remet le pool à zéro après distribution. |
| `calculate_fee_burn()` | `pms-economics/src/fee_burn.rs` | Calcule le montant à brûler vs distribuer selon `burn_rate_bps`. |
| `resolve_effective_fees()` | `pms-server/src/api_fn/tx_helpers.rs` | Merge les fees globales avec les overrides per-ledger. |
| `load_burn_rate_bps()` | `pms-server/src/api_fn/tx_helpers.rs` | Charge le taux de burn (priorité RuntimeConfig > EffectiveFees). |
| `increment_total_burned()` | `pms-storage/src/rocks_store/node_rewards_storage.rs` | Persiste le total cumulatif de fees brûlées dans RocksDB. |
| `get_total_burned()` | `pms-storage/src/rocks_store/node_rewards_storage.rs` | Lit le total cumulatif de fees brûlées. |
| `resolve_beneficiary_address()` | `pms-server/src/fee_distribution.rs` | Résout l'adresse d'un bénéficiaire par son rôle (coordinator, treasury, custom). |

## Endpoints API

| Méthode | Path | Description |
|---------|------|-------------|
| `GET` | `/v1/fee_pool` | Statut du pool de fees : `total_fees`, `total_burn_refunds`, `burn_refund_count`, `tx_count`, `num_contributors`. Public. |
| `POST` | `/admin/distribute_fees` | Déclenche une distribution manuelle. Coordinator-only (403 sinon). Body optionnel : `{ "parent_id": "..." }`. |
| `GET` | `/v1/supply` | Supply circulant avec champ `total_burned` (cumul des fees brûlées). |

### Format de réponse de `/admin/distribute_fees`

```json
{
  "success": true,
  "reward_block_id": "abc123...",
  "total_distributed": "42.50000000",
  "num_recipients": 3
}
```

### Format de réponse de `/v1/fee_pool`

```json
{
  "total_fees": "125.35000000",
  "total_burn_refunds": "2.50000000",
  "burn_refund_count": 1,
  "tx_count": 47,
  "num_contributors": 2
}
```

## Interactions

### Avec le système [[economics|Economics]] (fee burn)

Le module `pms-economics::fee_burn` est appelé dans `perform_fee_distribution()` pour calculer la fraction des fees à brûler. Le `burn_rate_bps` est configurable via TOML, per-ledger overrides (`LedgerFeesOverride`), et hot-swap (`ConfigUpdate::SetBurnRate`). Le total brûlé est persisté dans RocksDB CF `node_fee_pool` sous la clé `total_burned`.

### Avec le DAG (dépendance aux tips)

La distribution dépend de `top_tips(1)` pour obtenir un parent valide pour le bloc de distribution. Si le DAG est over-pruned (aucun tip disponible), la distribution est **BLOQUÉE** et les fees s'accumulent. Ce scénario a causé un bug critique en production (fees bloquées pendant des heures) corrigé par :
- Commit `9e2922f` : Protection du dernier tip dans la couche RAM (`prune_oldest()`)
- Commit `89e1a2f` : Protection du dernier tip dans la couche RocksDB (`trim_tips()`, `remove_tip()`)

### Avec les blocs chiffrés (EncryptedReward)

En production, les outputs de récompense peuvent être chiffrés individuellement pour chaque destinataire + Coordinator via X25519 (Diffie-Hellman). Les types `EncryptedRewardOutput` et `PlainPayload::EncryptedReward` supportent ce mode. Le `signer_x25519_hex` dans les métadonnées du bloc permet le déchiffrement.

### Avec la RuntimeConfig (hot-swap)

Les paramètres de distribution peuvent être modifiés sans redémarrage via des blocs `ConfigUpdate` signés par le Coordinator. La chaîne de priorité pour le chargement des paramètres est : `RuntimeConfig` (RocksDB) > `EffectiveFees` (per-ledger override) > `FeesSettings` (config TOML globale).

### Avec les [[smart-contracts|contrats smart]] (burn refunds)

Lorsqu'un contrat smart déclenche un burn refund (ex: `OnNftBurn`), le montant est ajouté au pool via `pool.add_burn_refund()`. Ces refunds sont distribués directement aux wallets utilisateurs lors de la prochaine distribution périodique, séparément des fees de nœuds.

**Note (v0.5.1)** : Avant cette version, les burn refunds ne fonctionnaient pas sur les custom ledgers. `evaluate_contracts_after_burn()` cherchait les contrats dans `state.store` (le RocksDB du ledger courant), mais les contrats sont stockés uniquement dans le store du main ledger. Les burns sur un custom ledger (ex: eden) ne trouvaient aucun contrat et ne produisaient donc aucun refund dans le FeePool. Le fix ajoute un champ `AppState.contract_store` qui pointe toujours vers le main store, utilisé par `evaluate_contracts_after_burn()` pour les lookups de contrats. Voir [[smart-contracts]] pour les details.

### Avec le Node Registry (rewards multi-nœuds)

Dans `perform_fee_distribution()`, les parts de chaque nœud sont calculées proportionnellement au nombre de blocs qu'ils ont créés. Le `node_registry` est consulté pour résoudre les adresses de wallet des nœuds. Si un nœud n'a pas d'adresse de wallet enregistrée, sa part est redirigée vers le Treasury (fallback de sécurité).

## Tests

| Fichier de test | Description |
|----------------|-------------|
| `crates/pms-server/tests/fee_distribution_e2e_test.rs` | Test E2E de la distribution complète (collecte, pool, distribution, vérification des UTXOs) |
| `crates/pms-server/tests/automated_distribution_test.rs` | Test de la distribution automatique avec mock adapter et timer |
| `crates/pms-server/tests/fee_consistency_test.rs` | Test de cohérence des fees (somme des outputs = total pool) |
| `crates/pms-server/tests/fee_treasury_test.rs` | Test du split coordinator/treasury et edge cases |
| `crates/pms-server/tests/fee_helpers_test.rs` | Tests des fonctions helper (load_*, resolve_effective_fees, etc.) |
| `crates/pms-server/src/fee_distribution.rs` (mod tests) | Tests unitaires : validation config, N-way split, block reward outputs |
| `crates/pms-server/src/fee_pool.rs` (mod tests) | Tests unitaires : shares proportionnelles, précision décimale |

## Column Families RocksDB

| CF | Clés | Description |
|----|------|-------------|
| `node_fee_pool` | `pool` (u64 LE) | Montant total du pool de fees persistant |
| `node_fee_pool` | `total_burned` (Decimal string) | Cumul des fees brûlées |
| `node_block_counts` | `<node_pk>` (u64 LE) | Nombre de blocs créés par nœud |
| `node_reward_addresses` | `<node_pk>` (string) | Adresse de wallet de récompense par nœud |

## Sécurité

- **Coordinator-only** : Seul le Coordinator peut déclencher une distribution (vérification de la clé publique).
- **Validation bps** : La somme des basis points doit être exactement 10000 (100%). Tout écart est rejeté.
- **Treasury wallets signés** : La liste des wallets treasury est signée par le Coordinator (ECDSA secp256k1) et vérifiée au démarrage.
- **Précision Decimal** : Tous les calculs financiers utilisent `rust_decimal::Decimal` avec arrondi à 8 décimales.
- **Fallback de sécurité** : Si un nœud n'a pas de wallet, sa part va au Treasury. Si aucun Treasury n'est configuré, les fonds restent dans le pool de nœuds.
- **Protection anti-tipless** : Le système refuse de distribuer si `top_tips()` retourne vide, évitant la création de blocs orphelins.
