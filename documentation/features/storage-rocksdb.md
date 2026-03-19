---
tags: [feature, infrastructure]
created: 2026-03-14
updated: 2026-03-17
version: v0.5.8
---

# Storage / RocksDB

## Resume

La couche de persistance RocksDB constitue le socle de stockage durable du moteur bancaire DAG-PMS. Elle garantit la durabilite des blocs, des UTXOs, des NFTs, des index d'activite, des contrats declaratifs, des gas pools et de la configuration runtime. Architecturee autour de **31 Column Families** par ledger, elle supporte le mode multi-ledger via prefixage dynamique des CFs, et integre un systeme de migrations incrementales (schema DB version + DAG protocol version). Les performances sont optimisees pour un throughput soutenu de 120+ TPS sur VPS 8 Go grace a un tuning RocksDB pousse (bloom filters sur toutes les CFs, block cache 512 Mo, prevention des write stalls L0, sub-compactions paralleles).

## Architecture

### Dual-Layer : RAM (ConcurrentDag) + RocksDB (RocksStore)

Le systeme utilise une architecture a deux couches complementaires :

```
+-------------------------------------------------------+
|                     API / Consensus                    |
+-------------------------------------------------------+
         |                              |
         v                              v
+-------------------+          +-------------------+
|   ConcurrentDag   |          |    RocksStore      |
|   (RAM, hot)      |          |   (SSD, durable)   |
+-------------------+          +-------------------+
| DashMap<id,Block> |          | 31 CFs x N ledgers |
| DashSet(outpoints)|          | WriteBatch atomic   |
| FinalityState     |          | Bloom + LRU cache   |
| Bounded by        |          | Unbounded (disque)  |
| max_dag_blocks    |          |                     |
+-------------------+          +-------------------+
```

**ConcurrentDag** (`crates/pms-core/src/concurrent_dag.rs`) :
- Stockage RAM borne (`max_dag_blocks`, defaut 50 000 blocs) pour les operations chaudes : selection de parents (tips), detection de double-spend, finality BFS
- Utilise `DashMap` (16 segments internes) pour des insertions concurrentes lock-free
- Pruning amorti (`prune_oldest()` toutes les 1000 insertions) : supprime les blocs les plus anciens non-tip
- Protection du dernier tip (commit `9e2922f`) : refuse de supprimer le dernier tip restant pour eviter le blocage de la distribution des fees

**RocksStore** (`crates/pms-storage/src/rocks_store/store.rs`) :
- Stockage durable illimite sur SSD via RocksDB (mode `MultiThreaded`)
- Ecritures atomiques via `WriteBatch` (block + index + tips + children + activity en une seule operation)
- Protection miroir du dernier tip dans `remove_tip()` et `trim_tips()` (meme logique que `prune_oldest()` cote RAM)
- Cache stale-while-revalidate sur `top_tips()` (5 secondes de staleness, evite les stampedes)
- Cache 500 ms sur `runtime_config` (evite un GET RocksDB + JSON deser par bloc)
- Cache in-memory DashSet pour les adresses gelees (`frozen_set`, O(1) sans I/O)
- `db_path: PathBuf` stocke le chemin absolu de la DB. Utilise pour deriver le chemin de backup (sibling `backups/pms/`) — garantit que les checkpoints sont toujours sur le meme volume que les donnees (critique en Docker)

### Regle critique : Dual-Layer Consistency

> **Tout fix applique sur une couche DOIT etre verifie et applique sur l'autre couche si la meme logique existe.**

Exemple historique : `prune_oldest()` (RAM) a ete corrige pour proteger le dernier tip, mais `trim_tips()` et `remove_tip()` (RocksDB) n'ont pas recu la meme protection. Resultat : fees bloquees pendant des heures en production.

### Multi-Prefix Mode (Multi-Ledger)

Chaque ledger dispose de son propre jeu de 31 CFs, prefixees par l'identifiant du ledger :
- Ledger principal : `mainnet:blocks`, `mainnet:utxo`, `mainnet:tips`, ...
- Ledger custom : `custom_ledger_42:blocks`, `custom_ledger_42:utxo`, ...

Deux modes d'ouverture :
1. **`RocksStore::new()`** : un seul prefix, DB dediee
2. **`RocksStore::open_db_multi_prefix()`** : N prefixes, DB partagee. Retourne un `Arc<PmsDb>` partage entre N instances `RocksStore` (via `from_shared_db()`)

La creation dynamique de CFs a runtime est supportee grace au mode `MultiThreaded` de RocksDB (`create_cf(&self, ...)` sans `&mut self`).

### Nommage des CFs

Les CFs utilisent le format `{prefix}:{cf_short_name}`. Le mapping short name -> full name est pre-calcule au bootstrap dans `cf_names: HashMap<String, String>` pour eliminer les allocations `format!()` sur le chemin critique (environ 11 appels `cf()` par bloc).

## Column Families

### Liste exhaustive (31 CFs par ledger)

| # | Nom CF | Cle | Valeur | Role | Module |
|---|--------|-----|--------|------|--------|
| 1 | `blocks` | `block_id` (bytes) | JSON(`StoredBlock`) | Stockage principal des blocs serialises | `store.rs` |
| 2 | `idx_blocks` | `block_id` (bytes) | `""` (vide) | Index d'existence rapide des blocs (set semantics) | `store.rs` |
| 3 | `by_time` | `[ts:8][block_id]` | `""` | Index chronologique (insertion order) pour pagination | `store.rs`, `atomic.rs` |
| 4 | `id2ts` | `block_id` (bytes) | `ts` (8 bytes BE) | Reverse lookup : block_id -> timestamp d'insertion | `store.rs`, `atomic.rs` |
| 5 | `final` | `block_id` (bytes) | `""` | Ensemble des blocs finalises (k-depth finality) | `store.rs` |
| 6 | `last_ms` | `"last"` (fixe) | `block_id` (bytes) | Dernier milestone emis | `store.rs` |
| 7 | `children_count` | `parent_id` (bytes) | `u64` (8 bytes LE) | Nombre d'enfants par bloc (pour poids tip selection) | `store.rs`, `atomic.rs` |
| 8 | `tips` | `block_id` (bytes) | `ts` (8 bytes BE) | Ensemble des tips actuels (feuilles du DAG) | `store.rs`, `atomic.rs` |
| 9 | `children_set` | `parent_id\x00child_id` | `""` | Aretes parent->enfant du DAG | `store.rs`, `atomic.rs` |
| 10 | `ver` | `"ver"` / `"dag_version"` | Version string | Version schema DB (entier) et version DAG (SemVer) | `migration.rs` |
| 11 | `utxo` | `txid#index` | JSON(`{addr, amt, ast?}`) | UTXOs non depenses (set UTXO courant) | `utxo.rs`, `store.rs` |
| 12 | `utxo_spent` | `txid#index` | `block_id` (bytes) | UTXOs depenses (tracabilite : quel bloc a consomme) | `store.rs` |
| 13 | `tx_applied` | `txid` (bytes) | `""` | Transactions deja appliquees (idempotence) | `utxo.rs` |
| 14 | `nft_ownership` | `token_id` (bytes) | `owner_address` (bytes) | Proprietaire courant de chaque NFT | `nft_storage.rs` |
| 15 | `nfts_by_owner` | `owner_address` (bytes) | JSON(`Vec<token_id>`) | Index inverse : adresse -> liste de NFTs possedes | `nft_storage.rs` |
| 16 | `nft_block_ids` | `token_id` (bytes) | `block_id` (bytes) | Reference au bloc contenant les metadonnees chiffrees du NFT | `nft_storage.rs` |
| 17 | `runtime_config` | `"current"` (fixe) | JSON(`RuntimeConfig`) | Configuration runtime courante (fee tiers, mint policy, etc.) | `config_storage.rs` |
| 18 | `config_history` | `timestamp:block_id` | JSON(`ConfigHistoryEntry`) | Historique des changements de configuration | `config_storage.rs` |
| 19 | `node_block_counts` | `node_pk` (bytes) | `u64` (8 bytes LE) | Compteur de blocs mines par noeud (pour distribution fees) | `node_rewards_storage.rs` |
| 20 | `node_fee_pool` | `"pool"` / `"total_burned"` | `u64` LE / Decimal string | Pool de fees accumule + total cumule des fees brulees | `node_rewards_storage.rs` |
| 21 | `node_reward_addresses` | `node_pk` (bytes) | `address` (bytes) | Adresse de recompense par noeud (optionnel, defaut = pk) | `node_rewards_storage.rs` |
| 22 | `token_registry` | `asset_id` (bytes) | JSON(`TokenMetadata`) | Registre des tokens custom (multi-asset) | `token_registry.rs` |
| 23 | `bridge_consumed` | - | - | Outpoints bridge consommes (anti-replay cross-ledger) | `store.rs` |
| 24 | `bridge_links` | - | - | Liens bridge entre ledgers | `store.rs` |
| 25 | `compliance_frozen` | `address` (bytes) | JSON(`FrozenEntry`) | Adresses gelees (compliance) | `compliance_registry.rs` |
| 26 | `compliance_log` | `block_id` (bytes) | JSON(`ComplianceLogEntry`) | Journal d'audit compliance (freeze/unfreeze actions) | `compliance_registry.rs` |
| 27 | `addr_activity` | `[addr][0x00][ts:8][block_id]` | `""` | Index d'activite par adresse (non type) | `store.rs`, `atomic.rs` |
| 28 | `addr_type_activity` | `[addr][0x00][cat:1][ts:8][block_id]` | `""` | Index d'activite par adresse et categorie (type) | `store.rs`, `atomic.rs` |
| 29 | `activity_items` | `[addr][0x00][ts:8][block_id]` | JSON(`Vec<StoredActivityItem>`) | Items d'activite pre-calcules (evite le re-parsing des blocs) | `store.rs`, `atomic.rs` |
| 30 | `contracts` | `contract_id` (bytes) | JSON(`Contract`) | Contrats declaratifs (smart contracts rule-based) | `contract_storage.rs` |
| 31 | `gas_pools` | `ledger_id` (bytes) | JSON(`GasPool`) | Gas pools par ledger (anti-spam pour ledgers custom) | `gas_pool_storage.rs` |
| 32 | `ledger_subscriptions` | `ledger_id` (bytes) | JSON(`LedgerSubscription`) | Abonnements annuels des ledgers custom | `store.rs` (CF declare) |

> **Note** : Le tableau contient 32 CFs car `CF_NAMES` en declare 31 + la CF `default` obligatoire de RocksDB. La liste hardcodee dans `new()` et la constante `CF_NAMES` doivent rester synchronisees.

### Double liste de CFs (piege historique)

`store.rs` contient **deux** listes de CFs qui doivent imperativement rester identiques :
1. `CF_NAMES` (constante `&[&str]`, ligne ~260) : utilisee par `open_db_multi_prefix()`, `build_cf_names()`, `compact_all()`, `ensure_column_families()`
2. Le tableau `required` dans `new()` (ligne ~135) : utilisee pour l'ouverture single-prefix

Un CF present dans l'une mais absent de l'autre provoque un crash RocksDB au demarrage.

## Configuration

### Section `[rocks]` du fichier TOML

| Champ | Type | Defaut | Description |
|-------|------|--------|-------------|
| `path` | `String` | (requis) | Chemin du dossier RocksDB sur disque |
| `prefix` | `String` | `""` | Namespace logique (ex: `"testnet"`, `"mainnet"`) |
| `tip_limit` | `usize` | `200` | Nombre maximum de tips conserves dans la CF `tips` |
| `max_dag_blocks` | `usize` | `50_000` | Blocs max en RAM (ConcurrentDag). 0 = illimite |
| `max_spent_outpoints` | `usize` | `500_000` | Outpoints depenses max en RAM (double-spend detection) |
| `max_utxos` | `usize` | `500_000` | UTXOs max dans le cache LRU RAM. Cache miss -> RocksDB |
| `checkpoint_interval_secs` | `Option<u64>` | `21600` (6h) | Intervalle entre les checkpoints de backup |
| `write_buffer_size_mb` | `usize` | `128` | Taille du memtable par CF, en MB. Reduire pour multi-ledger (ex: 64) |
| `max_write_buffer_number` | `i32` | `3` | Nombre max de memtables par CF avant flush |
| `block_cache_size_mb` | `usize` | `512` | Cache LRU partage entre toutes les CFs, en MB |
| `db_write_buffer_size_mb` | `usize` | `512` | Budget memtable global (toutes CFs). 0 = desactive. **Critique pour multi-ledger** |
| `max_open_files` | `i32` | `512` | Limite FD RocksDB. -1 = illimite (dangereux). **Critique pour VPS avec ulimit=1024 et 66+ CFs** (v0.5.8) |

### RocksMemoryConfig (v0.5.7, FD limit v0.5.8)

Structure dediee encapsulant les 5 parametres memoire/FD configurables, passee a `new()` et `open_db_multi_prefix()` :

```rust
pub struct RocksMemoryConfig {
    pub write_buffer_size_mb: usize,     // Per-CF memtable size
    pub max_write_buffer_number: i32,    // Max memtables per CF
    pub block_cache_size_mb: usize,      // Shared LRU cache
    pub db_write_buffer_size_mb: usize,  // Global memtable budget
    pub max_open_files: i32,             // FD limit (v0.5.8)
}
```

**Recommandations par taille VPS :**

| VPS | `write_buffer_size_mb` | `block_cache_size_mb` | `db_write_buffer_size_mb` |
|-----|----------------------|---------------------|------------------------|
| 4 GB RAM | 32 | 128 | 256 |
| 8 GB RAM (testnet) | 64 | 256 | 512 |
| 16 GB RAM | 128 (defaut) | 512 (defaut) | 1024 |

### Parametres `apply_db_tuning()` (configurable + hardcodes)

Ces parametres sont appliques uniformement a `new()` et `open_db_multi_prefix()` via la methode centralisee `apply_db_tuning()` :

| Parametre | Valeur | Configurable | Justification |
|-----------|--------|-------------|---------------|
| `create_if_missing` | `true` | Non | Cree la DB si elle n'existe pas |
| `create_missing_column_families` | `true` | Non | Cree les CFs manquantes au demarrage |
| `increase_parallelism` | `num_cpus` | Non | Un thread background par coeur CPU |
| `max_background_jobs` | `6` | Non | Flush + compaction overlap sur VPS 4-core |
| `level_compaction_dynamic_level_bytes` | `true` | Non | Ajuste automatiquement la taille des niveaux |
| `write_buffer_size` | `128 MB` | **Oui** (`write_buffer_size_mb`) | Taille du memtable avant flush |
| `max_write_buffer_number` | `3` | **Oui** (`max_write_buffer_number`) | Max memtables par CF |
| `db_write_buffer_size` | `512 MB` | **Oui** (`db_write_buffer_size_mb`) | Cap memoire global memtables (v0.5.7) |
| `target_file_size_base` | `64 MB` | Non | Taille cible par SSTable |
| `level_zero_file_num_compaction_trigger` | `4` | Non | Debut de compaction L0 (defaut) |
| `level_zero_slowdown_writes_trigger` | `40` | Non | Seuil de ralentissement (defaut: 20) |
| `level_zero_stop_writes_trigger` | `56` | Non | Seuil d'arret total (defaut: 24) |
| `max_subcompactions` | `3` | Non | Parallelise chaque job de compaction |
| `max_open_files` | `512` | **Oui** (`max_open_files`) | Limite FD RocksDB. Empêche FD exhaustion sur VPS (v0.5.8) |
| `advise_random_on_open` | `true` | Non | `POSIX_FADV_RANDOM` sur les SST files — désactive le readahead kernel (128 KB par défaut). Sans ce flag, le page cache Linux sature la limite Docker cgroup (v0.5.16) |
| `compaction_readahead_size` | `2 MB` | Non | Readahead séquentiel pour les jobs de compaction (nécessaire car `advise_random` désactive le readahead normal) (v0.5.16) |

### Block Cache et Bloom Filters

Appliques a **toutes** les 31 CFs (pas seulement aux CFs d'index) :

| Parametre | Valeur | Configurable | Justification |
|-----------|--------|-------------|---------------|
| `bloom_filter` | `10.0 bits/key, non full-key` | Non | Reduit les lectures disque sur point lookups |
| `block_cache` (LRU partage) | `512 MB` | **Oui** (`block_cache_size_mb`) | Partage entre toutes les CFs |
| `cache_index_and_filter_blocks` | `true` | Non | Index et filtres en cache (pas sur disque) |
| `pin_l0_filter_and_index_blocks_in_cache` | `true` | Non | Empeche l'eviction des blocs L0 |
| `optimize_filters_for_hits` | `true` | Non | Optimise les bloom filters pour les hits |

## Crates et Fichiers

| Crate | Fichier | Role |
|-------|---------|------|
| `pms-storage` | `src/rocks_store/store.rs` | Structure `RocksStore`, `CF_NAMES`, `apply_db_tuning()`, `new()`, `open_db_multi_prefix()`, `from_shared_db()`, `trim_tips()`, `top_tips()`, pagination |
| `pms-storage` | `src/rocks_store/mod.rs` | Declaration des sous-modules : `atomic`, `cf_operation`, `compliance_registry`, `config_storage`, `contract_storage`, `gas_pool_storage`, `helpers`, `migration`, `nft_storage`, `node_rewards_storage`, `token_registry`, `utxo` |
| `pms-storage` | `src/rocks_store/atomic.rs` | `append_block_atomic()`, `persist_genesis()`, `apply_dag_indices()`, `apply_addr_activity_indices()` |
| `pms-storage` | `src/rocks_store/cf_operation.rs` | Helper `cf()` : resolution short name -> handle CF (sans allocation) |
| `pms-storage` | `src/rocks_store/helpers.rs` | `flush_wal()`, `compact_all()`, `log_stats()`, `create_checkpoint()` |
| `pms-storage` | `src/rocks_store/migration.rs` | Migrations `mig_0_to_1()` a `mig_7_to_8()`, `ensure_schema()`, `get_version()`, `get_dag_version()` |
| `pms-storage` | `src/rocks_store/utxo.rs` | `utxo_apply_tx_atomic()`, `get_utxo()`, `iter_all_utxos()`, `stream_all_utxos()` |
| `pms-storage` | `src/rocks_store/nft_storage.rs` | Implementation de `NftStorage` : ownership, reverse index, block references |
| `pms-storage` | `src/rocks_store/node_rewards_storage.rs` | `NodeRewardsStorage` : pool fees, compteurs blocs, reward addresses, fee burn tracking |
| `pms-storage` | `src/rocks_store/compliance_registry.rs` | `ComplianceStorage` : freeze/unfreeze, audit log, frozen DashSet cache |
| `pms-storage` | `src/rocks_store/config_storage.rs` | `ConfigStorage` : runtime config (cached 500ms), config history |
| `pms-storage` | `src/rocks_store/token_registry.rs` | Token registry : register, get, list tokens custom (multi-asset) |
| `pms-storage` | `src/rocks_store/contract_storage.rs` | `ContractStorage` : CRUD contrats, recherche par trigger (scan complet, nombre faible) |
| `pms-storage` | `src/rocks_store/gas_pool_storage.rs` | `GasPoolStorage` : deposit, withdraw, consume gas (read-modify-write atomique) |
| `pms-storage` | `src/migrations.rs` | Constantes `CURRENT_VER` (8), `DAG_VERSION` ("1.2.0"), type `MigError` |
| `pms-storage` | `src/lib.rs` | `DagSemVer`, `VersionCheck`, `check_dag_compatibility()` |
| `pms-storage` | `src/traits.rs` | Trait `DagStorage` : interface abstraite pour le stockage DAG |
| `pms-storage` | `src/store.rs` | Enum `PutResult` : `Inserted`, `AlreadyExists`, `Rejected` |
| `pms-storage` | `src/checkpoint_rocks.rs` | `rotate_checkpoints()` : rotation des backups (garde les N plus recents) |
| `pms-storage` | `src/gas_pool_store.rs` | Trait `GasPoolStorage` |
| `pms-storage` | `src/contract_store.rs` | Trait `ContractStorage` |
| `pms-storage` | `src/compliance_store.rs` | Trait `ComplianceStorage` |
| `pms-storage` | `src/config_store.rs` | Trait `ConfigStorage` |
| `pms-storage` | `src/nft_store.rs` | Trait `NftStorage` |
| `pms-storage` | `src/node_rewards.rs` | Trait `NodeRewardsStorage` |
| `pms-storage` | `src/mutation.rs` | Enum `LedgerMutation` : abstraction des types de mutations |
| `pms-config` | `src/config.rs` | Structure `Rocks` : configuration TOML de la couche stockage |
| `pms-core` | `src/concurrent_dag.rs` | `ConcurrentDag` : couche RAM, `prune_oldest()`, tip selection |

## Fonctions Cles

### Initialisation et ouverture

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `RocksStore::new()` | `store.rs` | Ouverture single-prefix : cree les 31 CFs prefixees, applique le tuning, bloom filters sur toutes les CFs |
| `RocksStore::open_db_multi_prefix()` | `store.rs` | Ouverture multi-prefix : cree les CFs pour N ledgers, retourne `Arc<PmsDb>` partageable |
| `RocksStore::from_shared_db()` | `store.rs` | Cree un `RocksStore` a partir d'un `Arc<PmsDb>` deja ouvert (multi-ledger) |
| `RocksStore::open_read_only()` | `store.rs` | Ouvre la DB en lecture seule (pas de lock exclusif) |
| `RocksStore::open_secondary()` | `store.rs` | Ouvre une instance secondaire (replica read-only avec catch-up) |
| `apply_db_tuning()` | `store.rs` | Centralise le tuning RocksDB (L0 thresholds, parallelism, memtables) |
| `build_cf_names()` | `store.rs` | Pre-calcule le mapping short name -> `"prefix:name"` (elimine les allocations `format!()`) |
| `ensure_column_families()` | `store.rs` | Cree les CFs manquantes au bootstrap (supporte les upgrades de schema) |
| `load_frozen_cache()` | `store.rs` | Charge les adresses gelees dans le DashSet in-memory au demarrage |

### Ecriture atomique

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `append_block_atomic()` | `atomic.rs` | Insere un bloc avec tous ses index DAG en un seul `WriteBatch` atomique |
| `append_block_atomic_with_utxo()` | `store.rs` | Idem + delta UTXO (spend/create) dans le meme batch |
| `apply_dag_indices()` | `atomic.rs` | Ecrit les index DAG dans un `WriteBatch` existant (blocks, idx, time, tips, children) |
| `apply_addr_activity_indices()` | `atomic.rs` | Ecrit les index d'activite (addr_activity, addr_type_activity, activity_items) dans un batch |
| `persist_genesis()` | `atomic.rs` | Insere le bloc genesis via `append_block_atomic()` |
| `utxo_apply_tx_atomic()` | `utxo.rs` | Applique une transaction UTXO atomiquement : check existence -> delete inputs -> create outputs -> mark applied |

### Gestion des tips

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `add_tip()` | `store.rs` | Ajoute un tip, incremente l'estimateur, lance `trim_tips()` |
| `remove_tip()` | `store.rs` | Supprime un tip avec protection du dernier tip (refuse la suppression si `count <= 1`) |
| `trim_tips()` | `store.rs` | Elagage des tips excedentaires : tri par ts DESC, garde les `tip_limit` plus recents, protege au moins 1 tip |
| `maybe_trim_tips()` | `store.rs` | Amortissement : execute `trim_tips()` seulement toutes les 64 insertions |
| `top_tips()` | `store.rs` | Retourne les N tips les plus recents, avec cache stale-while-revalidate (5s) |

### Maintenance et backup

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `spawn_background_maintenance()` | `store.rs` | Tache async de maintenance : flush WAL, compaction, stats, checkpoints |
| `flush_wal()` | `helpers.rs` | Flush le WAL sur disque avec fsync |
| `compact_all()` | `helpers.rs` | Compacte toutes les CFs avec rate-limiting (200ms entre chaque CF) |
| `log_stats()` | `helpers.rs` | Log des statistiques RocksDB (L0 files, stalls, compactions pending) |
| `create_checkpoint()` | `helpers.rs` | Cree un snapshot RocksDB dans le dossier de backup |
| `rotate_checkpoints()` | `checkpoint_rocks.rs` | Rotation des backups : garde les N plus recents, supprime les anciens |
| `bootstrap_once_for_production()` | `store.rs` | Flush + compaction initiale au demarrage (optionnel) |

### Reindexation

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `reindex_all_activity()` | `store.rs` | Reconstruit `addr_activity` + `addr_type_activity` pour tous les blocs existants |
| `reindex_all_activity_items()` | `store.rs` | Reconstruit `activity_items` (pre-calcul) pour tous les blocs, avec flush par batch de 1000 |

### Queries paginee

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `recent_ids_by_time()` | `store.rs` | Scan reverse-chronologique de `by_time` avec curseur `(ts, id, has_more)` |
| `recent_ids_by_address()` | `store.rs` | Scan reverse-chronologique de `addr_activity` filtre par adresse |
| `recent_ids_by_address_and_categories()` | `store.rs` | Scan multi-categorie avec k-way merge (max 9 iterateurs) |
| `recent_activity_items_by_address()` | `store.rs` | Idem + batch-fetch des `activity_items` via `multi_get_cf()` |
| `ts_for_ids()` | `store.rs` | Batch lookup de timestamps depuis `id2ts` |

## Migrations

Le systeme de migration est gere par deux versioning independants :

### Schema DB Version (`CURRENT_VER`)

Version courante : **8** (definie dans `migrations.rs`)

Stockee dans la CF `ver` sous la cle `"ver"`. Incrementee a chaque nouveau schema de CFs ou d'index.

| Version | Migration | Description |
|---------|-----------|-------------|
| 0 -> 1 | `mig_0_to_1()` | Init : ecriture/suppression sentinelle dans `idx_blocks` (equivalent SADD/SREM Redis) |
| 1 -> 2 | `mig_1_to_2()` | Reconstruction des tips : pour chaque bloc, `add_tip(id)` puis `remove_tip(parents)` |
| 2 -> 3 | `mig_2_to_3()` | Reconstruction de `by_time` et `id2ts` perdus par `trim_by_time()`. Timestamps synthetiques pour les blocs anciens (tri avant les timestamps reels) |
| 3 -> 4 | `mig_3_to_4()` | Backfill de `addr_activity` : parse chaque bloc, extrait les adresses impliquees, ecrit les index |
| 4 -> 5 | `mig_4_to_5()` | Backfill de `addr_type_activity` : extrait les paires (adresse, categorie) pour les index types |
| 5 -> 6 | `mig_5_to_6()` | Correction du type Fee : reindexe les blocs TxUtxo dont la sortie fee etait classee comme Transfer au lieu de Fee. Supprime les anciennes entrees stales |
| 6 -> 7 | `mig_6_to_7()` | Verification de la CF `contracts` (smart contracts declaratifs). No-op fonctionnel, verification d'accessibilite |
| 7 -> 8 | `mig_7_to_8()` | Verification des CFs `gas_pools` + `ledger_subscriptions` (systeme economique). No-op fonctionnel |

Toutes les migrations sont **idempotentes** (safe a rejouer).

### DAG Protocol Version (`DAG_VERSION`)

Version courante : **"1.2.0"** (definie dans `migrations.rs`)

Stockee dans la CF `ver` sous la cle `"dag_version"`. Suit le Semantic Versioning :
- **MAJOR** : changement incompatible (refus de demarrer, migration manuelle requise)
- **MINOR** : nouvelles fonctionnalites backward-compatible (migration auto)
- **PATCH** : correctifs (migration auto)

Verification au demarrage via `check_dag_compatibility()` :
- `Compatible` : meme major, stored <= current
- `MajorMismatch` : breaking change (major differents)
- `Downgrade` : stored > current (DB creee par une version plus recente)

## Performance Tuning

### Historique des optimisations

#### v0.2.5 : Fix du TPS cliff 120 -> 20

**Cause racine** : Accumulation de fichiers L0 dans RocksDB atteignant les seuils par defaut apres 40-60 minutes de charge soutenue.

**Correctifs** :
- `apply_db_tuning()` centralise le tuning (identique pour `new()` et `open_db_multi_prefix()`)
- L0 slowdown trigger : 20 -> 40 (double le headroom)
- L0 stop trigger : 24 -> 56 (proportionnel)
- `max_subcompactions` = 3 (parallelise les jobs de compaction)
- `compact_all()` rate-limited (200ms entre CFs, total ~6.2s au lieu d'un burst)
- `trim_tips()` amorti : toutes les 64 insertions via `maybe_trim_tips()`
- `top_tips()` cache : stale-while-revalidate (5 secondes au lieu de 500ms TTL)
- Intervalle de compaction periodique passe a 6h

#### v0.2.6 : Bloom filters sur toutes les CFs

**Cause racine** : Seules 7/31 CFs avaient des bloom filters. Les point lookups sur les 24 autres degradaient progressivement avec la croissance des niveaux LSM.

**Correctifs** :
- Bloom filters (10 bits/key) appliques aux 31 CFs dans `new()` et `open_db_multi_prefix()`
- `optimize_filters_for_hits(true)` sur toutes les CFs

#### v0.2.7 : Pinning L0 + 512 MB cache

**Cause racine** : 31 CFs rivalisaient pour l'espace cache (256 MB). Les blocs index/filter de L0 se faisaient evincer, chaque point lookup necessitait 2+ lectures disque. TPS tombait a 0.

**Correctifs** :
- Cache LRU passe de 256 MB a **512 MB** (partage entre toutes les CFs)
- `cache_index_and_filter_blocks(true)` : index et filtres en cache
- `pin_l0_filter_and_index_blocks_in_cache(true)` : les blocs L0 ne sont jamais evinces du cache
- Ces parametres sont appliques a la fois dans `new()` et `open_db_multi_prefix()`

### Optimisations de chemin critique

| Optimisation | Gain | Detail |
|-------------|------|--------|
| `cf_names` HashMap | Elimine `format!()` par CF call | ~11 appels/bloc, pre-calcule au bootstrap |
| `tip_count_estimate` AtomicUsize | Evite le scan complet de `tips` | Fast-path dans `trim_tips()` quand estimate <= limit |
| `top_tips_cache` | Evite les scans repetes de `tips` | Stale-while-revalidate, 5s de staleness |
| `runtime_config_cache` | Evite GET + JSON deser par bloc | 500ms TTL, write-through sur set |
| `frozen_set` DashSet | Evite GET RocksDB par tx (adresses gelees) | Charge au bootstrap, O(1) lock-free |
| `maybe_trim_tips()` | Amortit `trim_tips()` toutes les 64 ops | Reduit le cout par bloc (meme le `AtomicLoad` est mesurable a 120 TPS) |
| `multi_get_cf()` | Batch reads en une seule operation | Utilise pour children_count, activity_items, get_blocks_by_ids |
| `WriteBatch` atomique | Regroupe toutes les ecritures par bloc | Block + index + tips + children + activity en un seul write |

### Tache de maintenance background

Lancee via `spawn_background_maintenance()` avec 4 timers independants :

| Timer | Intervalle defaut | Operation |
|-------|-------------------|-----------|
| Flush WAL | configurable | `flush_wal(true)` avec fsync |
| Compaction | 6h | `compact_all()` avec rate-limiting 200ms/CF |
| Stats | configurable | Log L0 files, write stalls, compaction pending |
| Checkpoint | `checkpoint_interval_secs` (defaut 6h) | Snapshot + rotation (garde 3 checkpoints). Path derive de `db_path` (sibling `backups/pms/`), overridable via `PMS_BACKUP_ROOT` env var |

## Type de DB Handle

```rust
pub type PmsDb = DBWithThreadMode<MultiThreaded>;
```

Le mode `MultiThreaded` permet `create_cf(&self, ...)` sans `&mut self`, ce qui autorise la creation dynamique de CFs a runtime pour les nouveaux ledgers.

## Serialisation

Toutes les valeurs structurees sont serialisees en **JSON** (via `serde_json`) pour la compatibilite et la debuggabilite. Les timestamps sont stockes en **big-endian 8 bytes** pour un tri lexicographique naturel dans les iterateurs RocksDB. Les compteurs sont en **little-endian 8 bytes** (convention standard Rust).

## Interactions

- [[dag-pruning]] : `prune_oldest()` (RAM) et `trim_tips()` / `trim_by_time()` (RocksDB) — doivent rester synchronises
- [[multi-ledger]] : `open_db_multi_prefix()` pour N ledgers dans une seule DB RocksDB
- [[activity-system]] : CFs `addr_activity`, `addr_type_activity`, `activity_items` — indexation a l'insertion, pagination reverse-chronologique
- [[nft-system]] : CFs `nft_ownership`, `nfts_by_owner`, `nft_block_ids` — ownership tracking avec index inverse
- [[fee-distribution]] : CFs `node_fee_pool`, `node_block_counts`, `node_reward_addresses` — pool de fees distribue aux mineurs
- [[compliance]] : CFs `compliance_frozen`, `compliance_log` — gel d'adresses avec cache DashSet
- [[smart-contracts]] : CF `contracts` — contrats declaratifs rule-based
- [[economics]] : CFs `gas_pools`, `ledger_subscriptions` — anti-spam et abonnements ledgers
- [[token-registry]] : CF `token_registry` — multi-asset support
- [[bridge]] : CFs `bridge_consumed`, `bridge_links` — transferts cross-ledger
- [[runtime-config]] : CFs `runtime_config`, `config_history` — configuration hot-swappable
