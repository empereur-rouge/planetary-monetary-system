---
tags: [feature, core, performance]
created: 2026-01-08
updated: 2026-06-13
version: v0.11.1
---

# UTXO System

## Resume

Le systeme UTXO (Unspent Transaction Output) de PMS est une implementation shardee haute performance qui gere l'ensemble des sorties de transactions non depensees en RAM, avec fallback transparent vers RocksDB. Il constitue le coeur du modele transactionnel du moteur bancaire : chaque transfert consomme des UTXOs existants (inputs) et en cree de nouveaux (outputs), garantissant la conservation des fonds et la detection du double-spend.

L'architecture repose sur `ShardedUtxoSet`, un ensemble partitionne en 256 shards (un par premier octet du TxId), chacun protege par un `RwLock` independant et borne par un `LruCache`. Cette conception permet un acces concurrent massif sans lock global, condition necessaire pour atteindre un TPS eleve dans une architecture DAG de type IOTA.

Quatre caches secondaires sont maintenus de maniere incrementale pour eviter les scans complets :
- **`supply_cache`** : supply total par asset (PMS natif + tokens custom), mis a jour a chaque add/remove.
- **`native_balance_cache`** : balance PMS native par adresse, O(1) via `DashMap` lock-free.
- **`token_balance_cache`** (v0.6.7) : balance par `(address, asset_id)` pour tokens custom (EDN, etc.), O(1) via `DashMap` lock-free. Meme pattern que `native_balance_cache` mais cle par paire `(Arc<str>, Arc<str>)`.
- **`address_index`** : index inverse adresse -> set d'`OutputId`, pour les requetes UTXO par adresse.

## Architecture

```
+-------------------------------------------------------------+
| ShardedUtxoSet                                              |
|                                                             |
|  shards[0..255]: RwLock<LruCache<OutputId, CompactOutput>>  |
|    - Shard index = premier octet hex du TxId                |
|    - LRU borne (configurable, defaut 500 000)               |
|    - Eviction silencieuse (RocksDB = source de verite)      |
|                                                             |
|  address_index: DashMap<String, DashSet<OutputId>>          |
|    - Index inverse pour requetes par adresse                |
|                                                             |
|  supply_cache: Mutex<HashMap<Option<String>, (Decimal, n)>> |
|    - Supply incremental par asset_id                        |
|                                                             |
|  native_balance_cache: DashMap<String, Decimal>             |
|    - Balance PMS native O(1), lock-free                     |
|                                                             |
|  token_balance_cache: DashMap<(Arc<str>,Arc<str>), Decimal> |
|    - Balance token custom O(1) par (addr, asset_id)         |
|                                                             |
|  interner: Interner (Mutex<HashSet<Arc<str>>>)              |
|    - Deduplication addresses et asset_ids                   |
|                                                             |
|  fallback: Option<UtxoFetcher>                              |
|    - Closure (txid, index) -> Option<TxOutput> vers RocksDB |
+-------------------------------------------------------------+
```

## Representation Compacte en RAM

La structure `CompactOutput` reduit l'empreinte memoire de ~148 bytes (`TxOutput` avec allocations heap) a ~32 bytes par UTXO :

| Champ | Type | Taille | Strategie |
|---|---|---|---|
| `address` | `Arc<str>` | 8 bytes (pointeur) | Interning : partage entre tous les UTXOs d'une meme adresse |
| `amount` | `Decimal` | 16 bytes (stack) | Remplacement de `String` par `rust_decimal::Decimal` stack-allocated |
| `asset_id` | `Option<Arc<str>>` | 8 bytes | `None` = PMS natif (0 heap), `Some` = interning |

L'`Interner` utilise un `Mutex<HashSet<Arc<str>>>` pour deduplication. La contention est quasi nulle car seul le Coordinator ecrit en mode Single Writer.

## Configuration

| Parametre | Fichier | Section | Defaut | Description |
|---|---|---|---|---|
| `max_utxos` | `config.*.toml` | `[rocks]` | `2_000_000` | Capacite maximale du cache LRU (repartie sur 256 shards). `0` = illimite. (~64 MB RAM) |
| `max_spent_outpoints` | `config.*.toml` | `[rocks]` | `500_000` | Outpoints depenses en RAM pour detection double-spend. |

## Crates et Fichiers

| Crate | Fichier | Role |
|---|---|---|
| `pms-core` | `crates/pms-core/src/utxo.rs` | `ShardedUtxoSet`, `CompactOutput`, `Interner`, `UtxoFetcher` |
| `pms-core` | `crates/pms-core/src/concurrent_dag.rs` | `ConcurrentDag` avec `spent_outpoints: DashSet` pour double-spend en RAM |
| `pms-core` | `crates/pms-core/src/validations/transactions.rs` | `utxo_no_double_spend()`, `utxo_sufficient_funds()`, `validate_transaction_async()` |
| `pms-core` | `crates/pms-core/src/validations/apply.rs` | Application des diffs UTXO apres validation |
| `pms-wallet` | `crates/pms-wallet/src/utils/utxo_store.rs` | `gather_wallet_utxos_dec()`, `select_utxos_dec()`, `gather_address_utxos_dec()` |
| `pms-wallet` | `crates/pms-wallet/src/helpers.rs` | `build_utxo_tx_with_fee_checked()` - construction de transactions UTXO avec fees |
| `pms-config` | `crates/pms-config/src/settings.rs` | `Rocks.max_utxos`, `Rocks.max_spent_outpoints` |
| `pms-storage` | `crates/pms-storage/src/rocks_store/store.rs` | Persistance RocksDB des UTXOs (source de verite) |

## Fonctions Cles

| Fonction | Fichier | Description |
|---|---|---|
| `ShardedUtxoSet::new(max_utxos, fallback)` | `utxo.rs` | Constructeur. `max_utxos=0` = unbounded. Repartit la capacite sur 256 shards. |
| `ShardedUtxoSet::get(out_point)` | `utxo.rs` | Lecture concurrente via `peek` (pas de mutation LRU). Cache miss -> fallback RocksDB. |
| `ShardedUtxoSet::add(out_point, output)` | `utxo.rs` | Ajout avec compactage, mise a jour supply/balance/index. Eviction LRU silencieuse. |
| `ShardedUtxoSet::remove(out_point)` | `utxo.rs` | Spend d'un UTXO. Si evince du LRU, fallback RocksDB pour les mises a jour supply/balance. |
| `ShardedUtxoSet::apply_diff(spends, creates)` | `utxo.rs` | Application batch d'un delta (spend + create). Groupement par shard pour minimiser les locks. Index DashMap differe apres release du shard lock (previent deadlock). |
| `ShardedUtxoSet::circulating_supply()` | `utxo.rs` | Retourne le supply PMS natif depuis le cache incremental. O(1). |
| `ShardedUtxoSet::balance_by_address(addr)` | `utxo.rs` | Balance PMS native via `native_balance_cache`. O(1), pas de shard lock. |
| `ShardedUtxoSet::balance_by_address_and_asset(addr, asset)` | `utxo.rs` | Balance par asset. Native = O(1) via `native_balance_cache`. Token = O(1) via `token_balance_cache` (v0.6.7). |
| `ShardedUtxoSet::utxos_by_address(addr)` | `utxo.rs` | Tous les UTXOs d'une adresse. Groupement par shard. Cache miss -> fallback RocksDB. |
| `ShardedUtxoSet::rebuild_indexes()` | `utxo.rs` | Reconstruction post-bootstrap depuis le contenu des shards LRU. |
| `ShardedUtxoSet::rebuild_indexes_from_utxos(all)` | `utxo.rs` | Reconstruction post-bootstrap depuis une liste complete (quand > capacite LRU). |
| `validate_transaction_async(utxos, tx)` | `transactions.rs` | Validation UTXO async sans lock global DAG. Double-spend + conservation par asset. |
| `validate_bridge_lock_async(utxos, inputs, amount, asset)` | `transactions.rs` | Validation des inputs BridgeLock. Sum(inputs) >= amount + meme asset_id. |
| `gather_wallet_utxos_dec(store, ...)` | `utxo_store.rs` | Scan des blocs pour reconstituer les UTXOs d'un wallet (avec dechiffrement). |
| `select_utxos_dec(utxos, need)` | `utxo_store.rs` | Selection gloutonne (greedy) des UTXOs les plus gros d'abord pour couvrir un montant. |
| `build_utxo_tx_with_fee_checked(...)` | `helpers.rs` | Construction complete d'une transaction UTXO avec calcul de fee et change. |

## Interactions

- **[[wallet-encryption]]** : Le wallet utilise `gather_wallet_utxos_dec()` pour reconstituer ses UTXOs non depenses, en dechiffrant les payloads encrypted via X25519.
- **[[config-system]]** : `max_utxos` et `max_spent_outpoints` sont configures dans `[rocks]` et repris dans `ShardedUtxoSet::new()`.
- Le `ConcurrentDag` dans `concurrent_dag.rs` maintient un `DashSet<(String, u32)>` pour la detection rapide du double-spend en RAM, complementaire au `ShardedUtxoSet`.
- La validation async (`validate_transaction_async`) interroge le `ShardedUtxoSet` pour verifier l'existence des inputs et la conservation des montants par asset, sans lock global sur le DAG.
- Les operations de shard lock sont ordonnees (deferred DashMap updates) dans `apply_diff` pour prevenir les deadlocks entre `shard write lock -> DashMap` et `DashMap -> shard read lock`.
- **Idempotence du persist (v0.11.1, audit S4)** : `CoreAdapter::persist_block` deduplique le bloc (`ConcurrentDag::contains_block` -> `AlreadyExists`) **AVANT** d'appeler `apply_diff`. Re-soumettre un bloc deja persiste (re-gossip reseau, retry client, replay) ne ré-applique donc PAS son `UtxoDelta` -> pas de double-credit / inflation de supply. L'ordre inverse (apply puis dedup) etait un bug de la meme classe que le double-apply v0.7.20. Verrouille par `crates/pms-core/tests/dag_integrity.rs::duplicate_block_is_idempotent_no_double_apply` et la determinism/proof-of-reserves par `crates/pms-core/tests/replay_determinism.rs`.

## Decisions Techniques

1. **256 shards par premier octet hex** : Distribution uniforme des TxIds (SHA-256). Un seul byte suffit pour 256 buckets, sans modulo couteux.
2. **LRU borne avec fallback RocksDB** : Permet de borner la RAM a ~50 MB pour 500k UTXOs tout en garantissant la correction via RocksDB.
3. **Caches incrementaux (supply, balance)** : Evitent les full scans O(n) qui causeraient un stall du pipeline de validation. Le `native_balance_cache` est lock-free (`DashMap`) pour eviter la famine de read lock pendant le minting massif.
4. **Interning via `Arc<str>`** : Reduit de ~4.6x l'empreinte memoire par UTXO. Essentiel pour supporter des millions d'UTXOs en RAM.
5. **Deferred index updates** : Les mises a jour DashMap (address_index) sont collectees pendant le shard lock et appliquees apres release, evitant l'inversion de lock order qui causait des deadlocks en production (fix `f21a510`).
6. **Token balance cache (v0.6.7)** : Meme pattern que `native_balance_cache` mais cle composite `(Arc<str>, Arc<str>)` pour supporter le multi-asset. Utilise des `Arc<str>` interned pour eviter les allocations par lookup. Zero-balance cleanup dans `supply_sub_compact()` pour prevenir les fuites memoire. Rebuild complet dans `rebuild_indexes()` / `rebuild_indexes_from_utxos()` pour coherence post-bootstrap.
