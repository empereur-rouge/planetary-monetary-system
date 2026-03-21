---
tags: [feature]
created: 2026-02-17
updated: 2026-03-21
version: v0.6.0
---

# DAG Pruning (Gestion Mémoire du DAG)

## Résumé

Le DAG Pruning est un mécanisme d'éviction FIFO (first-in-first-out) qui borne la croissance mémoire du DAG en RAM. Sans lui, `ConcurrentDag` conservait TOUS les blocs via `DashMap`, causant une croissance mémoire illimitée (~2+ GB après 24h sur testnet avec 97 agents concurrents).

Le pruning fonctionne en **dual-layer** :

1. **RAM (ConcurrentDag)** : Éviction par ordre d'insertion des blocs les plus anciens via `prune_oldest()`, déclenchée de façon amortie toutes les 1000 insertions.
2. **RocksDB (RocksStore)** : Trim des tips excédentaires via `trim_tips()` / `maybe_trim_tips()`, amortie toutes les 64 persistances de blocs.

Les blocs prunés du RAM restent dans RocksDB pour les requêtes historiques (API, export). Le pruning ne touche jamais aux `spent_outpoints` (nécessaires pour la détection de double-spend).

## Dates

| | Date |
|---|---|
| Créée | 2026-02-17 |
| Dernière mise à jour | 2026-03-12 |
| Version d'introduction | v0.1.0 (commit `0eb79c1`) |

## Architecture

### Couche RAM : `ConcurrentDag`

Le `ConcurrentDag` maintient un `VecDeque<BlockId>` (`insertion_order`) qui enregistre l'ordre chronologique d'insertion. Quand le nombre de blocs dépasse `max_blocks`, les blocs les plus anciens (en tête de la deque) sont évictés.

**Cycle de pruning (`prune_oldest`)** :

```
Phase 1 : Collecte des IDs à supprimer (sous lock du Mutex insertion_order)
  - Pop depuis le front de la deque
  - Saute les "ghost entries" (blocs déjà supprimés)
  - Protège le dernier tip restant (push_back au lieu de supprimer)
  - Cap de sécurité : max itérations = taille de la deque

Phase 2 : Suppression des DashMaps (lock-free, hors du Mutex)
  - blocks.remove(id)
  - children_count.remove(id)
  - children_idx.remove(id)
  - tips.remove(id)

Phase 3 : Nettoyage de la FinalityState
  - finality.finalized.remove(id) pour borner le HashSet (~340 MB économisés)
```

**Déclenchement amorti** : `insert_block()` incrémente un compteur atomique (`insert_counter`). Toutes les `PRUNE_CHECK_INTERVAL` (1000) insertions, `prune_oldest()` est appelée. Le bootstrap utilise un chemin séparé (`bootstrap_insert`) qui ne déclenche pas de pruning intermédiaire.

### Couche RocksDB : `RocksStore`

RocksDB possède une column family `tips` qui stocke les tips actifs avec leur timestamp. Deux mécanismes de pruning :

1. **`trim_tips()`** : Scan complet de la CF `tips`, tri par timestamp descendant, suppression des tips les plus anciens au-delà de `tip_limit`. Conserve toujours au moins 1 tip (`keep = tip_limit.max(1)`).

2. **`maybe_trim_tips()`** : Amortise `trim_tips()` toutes les `TRIM_TIPS_INTERVAL` (64) persistances de blocs via un compteur atomique (`persist_counter`).

3. **`remove_tip()`** : Refuse de supprimer le dernier tip restant (protection identique à celle du RAM).

4. **`top_tips()` cache** : Stale-while-revalidate avec fenêtre de 5 secondes pour éviter les scans répétés de la CF `tips`.

### Bootstrap : Chargement chronologique sélectif (v0.5.13)

`bootstrap_from_store_with_capacity()` utilise un chargement en 4 phases :

```
Phase 1 : Chargement sélectif chronologique
  - Si max_blocks > 0 et DB contient plus de max_blocks :
    - newest_block_ids_by_time(max_blocks) : reverse iterator sur by_time CF
    - Retourne les N blocs les plus récents par timestamp d'insertion
    - Fallback sur newest_block_ids() (lexicographique) si by_time CF vide
  - Chaque bloc est inséré via bootstrap_insert() (pas de pruning intermédiaire)

Phase 2 : Ghost cleanup
  - cleanup_ghost_entries() : supprime les entries de children_count,
    children_idx et tips pour les IDs absents de blocks DashMap
  - Élimine les "parents fantômes" référencés par les blocs chargés
    mais hors de la fenêtre de chargement

Phase 3 : Construction de insertion_order
  - Utilise directement l'ordre de chargement (oldest-first)
  - Pas de re-tri lexicographique

Phase 4 : Prune unique post-chargement
  - prune_oldest() avec children_counts corrects et sans ghost entries
```

**Pourquoi chronologique ?** Les IDs de blocs sont des hashes (`compute_block_id`), donc l'ordre lexicographique est aléatoire. Avec 13.2M blocs et 50K chargés, le tri lexicographique sélectionnait des blocs aléatoires → 99.8% tips orphelins (49,904/50,000). Le tri chronologique préserve la localité parent-enfant → ~2-5% orphelins.

**Fallback** : Si la CF `by_time` est vide (cas de `import_json()` qui n'écrit pas dans cette CF), le bootstrap utilise l'ancien tri lexicographique avec un log `warn`.

**Économie mémoire** : Ghost cleanup après Phase 1 libère les entries DashMap pour les ~49K parents hors fenêtre (~100-800 MB selon le cas).

## Configuration

### `[rocks]` dans le fichier TOML

| Paramètre | Défaut | Description |
|-----------|--------|-------------|
| `max_dag_blocks` | `50000` | Nombre max de blocs en RAM dans le ConcurrentDag. `0` = illimité. |
| `max_spent_outpoints` | `500000` | Nombre max de spent outpoints en RAM. `0` = illimité. |
| `tip_limit` | `200` | Nombre max de tips conservés dans la CF RocksDB `tips`. |

### Constantes hardcodées

| Constante | Valeur | Fichier | Rôle |
|-----------|--------|---------|------|
| `PRUNE_CHECK_INTERVAL` | `1000` | `concurrent_dag/pruning.rs` | Pruning RAM amorti toutes les N insertions |
| `TRIM_TIPS_INTERVAL` | `64` | `store.rs` | Trim tips RocksDB amorti toutes les N persistances |
| `MAX_TIPS_CAP` | `64` | `concurrent_dag/tips.rs` | Nombre max de tips retournés par `find_tips()` |
| `TIP_CHILDREN_THRESHOLD` | `4` | `concurrent_dag/tips.rs` | Seuil de children pour la pondération des tips |

### Exemple de configuration

```toml
[rocks]
path = "/home/pms/data/rocks"
prefix = "pms:test"
tip_limit = 256
max_dag_blocks = 50000
max_spent_outpoints = 500000
checkpoint_interval_secs = 3600
```

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-core` | `crates/pms-core/src/concurrent_dag/mod.rs` | Struct `ConcurrentDag`, constructors, `insert_block()` |
| `pms-core` | `crates/pms-core/src/concurrent_dag/pruning.rs` | `prune_oldest()`, pruning FIFO logic |
| `pms-core` | `crates/pms-core/src/concurrent_dag/bootstrap.rs` | `bootstrap_insert()`, `bootstrap_from_store_with_capacity()`, `cleanup_ghost_entries()` |
| `pms-core` | `crates/pms-core/src/concurrent_dag/tips.rs` | `find_tips()`, tip DashSet management |
| `pms-core` | `crates/pms-core/src/concurrent_dag/spent.rs` | `mark_spent()`, spent outpoints management |
| `pms-core` | `crates/pms-core/src/concurrent_dag/finality.rs` | FinalityState integration |
| `pms-storage` | `crates/pms-storage/src/rocks_store/store.rs` | Pruning RocksDB : `trim_tips()`, `maybe_trim_tips()`, `remove_tip()`, `top_tips()` |
| `pms-storage` | `crates/pms-storage/src/rocks_store/atomic.rs` | Appel de `maybe_trim_tips()` après chaque `append_block_atomic()` |
| `pms-config` | `crates/pms-config/src/config.rs` | Struct `Rocks` : `max_dag_blocks`, `max_spent_outpoints`, `tip_limit` |
| `pms-ledger` | `crates/pms-ledger/src/instance.rs` | `LedgerInstance::bootstrap()` : passe `max_dag_blocks` à `bootstrap_from_store_with_capacity()` |
| `pms-ledger` | `crates/pms-ledger/src/manager.rs` | `LedgerManager` : propage `max_dag_blocks` vers les instances |
| `pms-server` | `crates/pms-server/src/api/routes.rs` | `sync_dag_size_metric()` : synchronise la gauge Prometheus `pms_blocks_total` avec `dag.len()` |
| `pms-server` | `crates/pms-server/src/metrics.rs` | `PMS_BLOCKS_TOTAL` : gauge Prometheus reflétant la taille RAM du DAG |
| `pms-server` | `crates/pms-server/src/fee_distribution/distribute.rs` | [[fee-distribution|Distribution des frais]] : dépend de `top_tips()` pour trouver un parent |
| `pms-core` | `crates/pms-core/tests/bootstrap_pruning.rs` | Tests d'intégration pour `bootstrap_from_store_with_capacity` avec mock store |

## Fonctions Clés

### Couche RAM (`crates/pms-core/src/concurrent_dag/`)

| Fonction | Description |
|----------|-------------|
| `ConcurrentDag::with_capacity(max_blocks)` | Constructeur avec limite de blocs. `0` = illimité. |
| `ConcurrentDag::with_capacity_and_spent_limit(max_blocks, max_spent)` | Constructeur avec limites sur blocs ET spent outpoints. |
| `ConcurrentDag::insert_block(block)` | Insertion non-bloquante. Incrémente `insert_counter`, déclenche `prune_oldest()` toutes les 1000 insertions. Met à jour le `tips` DashSet incrémentalement. |
| `ConcurrentDag::prune_oldest()` | Éviction FIFO. Phase 1 sous Mutex (collecte IDs), Phase 2 lock-free (suppression DashMaps), Phase 3 nettoyage FinalityState. Protège le dernier tip. |
| `ConcurrentDag::bootstrap_insert(block)` | Insertion pendant le bootstrap. Construit `blocks`, `children_count`, `children_idx`, `tips` SANS tracker `insertion_order` ni déclencher de pruning. |
| `ConcurrentDag::bootstrap_from_store_with_capacity(store, max_blocks, max_spent)` | Chargement chronologique sélectif depuis RocksDB + ghost cleanup + prune finale. |
| `ConcurrentDag::cleanup_ghost_entries()` | Post-bootstrap : supprime les entries DashMap pour les parents hors de la fenêtre chargée. |
| `ConcurrentDag::bootstrap_from_store(store)` | Wrapper sans limite (capacity=0, pour tests/CLI). |
| `ConcurrentDag::find_tips()` | Retourne les tips depuis le `DashSet` incrémental (O(tips) au lieu de O(all_blocks)). |
| `ConcurrentDag::mark_spent(txid, index)` | Marque un outpoint comme dépensé. Si `max_spent_outpoints > 0`, éviction FIFO des plus anciens. |

### Couche RocksDB (`crates/pms-storage/src/rocks_store/store.rs`)

| Fonction | Description |
|----------|-------------|
| `RocksStore::trim_tips()` | Scan complet de la CF `tips`, tri par timestamp DESC, suppression des tips excédentaires via WriteBatch. Conserve toujours au moins 1 tip. Fast-path via `tip_count_estimate`. |
| `RocksStore::maybe_trim_tips()` | Amortise `trim_tips()` toutes les 64 persistances (`persist_counter`). |
| `RocksStore::add_tip(id)` | Ajoute un tip dans la CF, incrémente `tip_count_estimate`, appelle `trim_tips()`. |
| `RocksStore::remove_tip(id)` | Supprime un tip mais REFUSE si c'est le dernier restant (protection anti-tipless). |
| `RocksStore::top_tips(limit)` | Retourne les N tips les plus récents avec cache stale-while-revalidate (5s). |
| `RocksStore::is_empty()` | O(1) — single iterator seek sur `idx_blocks` CF. Override de `DagStorage::is_empty()`. |
| `RocksStore::newest_block_ids_by_time(n)` | Reverse iterator sur `by_time` CF pour chargement chronologique. Fallback sur lexicographique si `by_time` vide. |
| `RocksStore::block_count_estimate()` | O(1) via `rocksdb.estimate-num-keys` property. Fallback sur `block_count()`. |

### Métriques (`crates/pms-server/src/api/routes.rs` + `crates/pms-server/src/metrics.rs`)

| Fonction | Description |
|----------|-------------|
| `sync_dag_size_metric(st)` | Synchronise `PMS_BLOCKS_TOTAL` avec `dag.len()` pour le [[multi-ledger|ledger]] par défaut. Appelée à chaque fetch de `/metrics`. |
| `sync_dag_size_metric_for(st, ledger_id)` | Idem pour un [[multi-ledger|ledger]] spécifique. |
| `sync_all_dag_size_metrics(st)` | Synchronise pour tous les [[multi-ledger|ledgers]]. |

## Historical Bugs

Le DAG Pruning a connu une série de bugs critiques découverts progressivement en production entre le 17 et le 19 février 2026, puis ré-émergés le 1er et le 11 mars 2026 sur la couche RocksDB.

### Bug 1 : Tip-skipping causant une croissance illimitée (corrigé `5ce249f`, 2026-02-19)

**Problème** : La version initiale de `prune_oldest()` sautait systématiquement tous les tips (blocs sans enfants) pour les préserver pour la sélection de parents. Avec 97 agents concurrents créant ~96 tips orphelins par tick, la file `insertion_order` se remplissait de tips impossibles à évincer. Le DAG croissait sans limite malgré la configuration `max_dag_blocks`.

**Correction** : Suppression du tip-skipping. TOUS les blocs sont éligibles au pruning, y compris les tips. Les tips récents survivent naturellement car ils sont en queue de la deque. Seule exception : le **dernier tip restant** est protégé (push_back) pour garantir qu'un tip existe toujours.

### Bug 2 : Ghost entries bloquant le pruning (corrigé `f0645ab`, 2026-02-18)

**Problème** : Après un premier cycle de pruning, les IDs des blocs supprimés restaient dans `insertion_order` comme des "fantômes". Le cycle suivant tentait de les supprimer à nouveau, les comptait comme des évictions réussies, et n'atteignait jamais le quota réel.

**Correction** : Ajout d'un check `blocks.contains_key(&old_id)` avant de compter une éviction. Les ghost entries sont simplement sautées sans être comptées.

### Bug 3 : Écrasement de `children_count` pendant le bootstrap (corrigé `f0645ab`, 2026-02-18)

**Problème** : `all_block_ids()` retourne les IDs en ordre lexicographique (RocksDB), pas chronologique. Un enfant pouvait être chargé avant son parent. Quand le parent était ensuite inséré, `insert_block()` écrasait son `children_count` à 0 avec `AtomicU64::new(0)`, perdant le compteur incrémenté par l'enfant.

**Correction** : Remplacement de `insert(key, AtomicU64::new(0))` par `entry(key).or_insert_with(|| AtomicU64::new(0))`. Le compteur existant est préservé si un enfant l'a déjà incrémenté.

### Bug 4 : False-tip pollution pendant le bootstrap (corrigé `c714512`, 2026-02-18)

**Problème** : L'appel de `insert_block()` pendant le bootstrap déclenchait le pruning intermédiaire (via `PRUNE_CHECK_INTERVAL`). Comme les blocs étaient chargés dans un ordre aléatoire (lexicographique), le pruning intermédiaire évinçait des blocs dont les enfants n'étaient pas encore chargés, créant de faux tips.

**Correction** : Introduction de `bootstrap_insert()`, une méthode dédiée qui construit la structure du DAG (blocks, children_count, children_idx, tips) sans tracker `insertion_order` et sans déclencher de pruning. Après le chargement complet, `insertion_order` est populée explicitement et un seul `prune_oldest()` est exécuté avec des `children_counts` corrects.

### Bug 5 : Poisoned mutex (corrigé `592d1c0`, 2026-02-18)

**Problème** : Si un panic survenait pendant qu'un thread tenait le lock de `insertion_order`, le Mutex devenait "poisoned". Tous les appels subséquents à `prune_oldest()` échouaient silencieusement, désactivant le pruning indéfiniment.

**Correction** : Utilisation de `poisoned.into_inner()` pour récupérer le MutexGuard même après un poison. Ajout d'un log `tracing::error` pour signaler l'événement. Appliqué à la fois dans `prune_oldest()`, `insert_block()`, `new_with_genesis()` et `bootstrap_from_store_with_capacity()`.

### Bug 6 : DAG tipless causant le blocage de la [[fee-distribution|distribution des frais]] (corrigé `9e2922f`, 2026-03-01)

**Problème** : `prune_oldest()` pouvait supprimer TOUS les tips si le DAG ne contenait que des tips orphelins (scénario de 97 agents concurrents). `find_tips()` retournait un vecteur vide, ce qui bloquait silencieusement `perform_fee_distribution()` car `top_tips(1)` ne trouvait aucun parent. Sur testnet, 231k PMS de frais ont été bloqués pendant des heures.

**Correction** : Ajout d'une protection "last tip" : quand un bloc candidat au pruning est un tip et que `live_tips <= 1`, il est push_back en queue de la deque au lieu d'être supprimé. Un cap de sécurité (`max_iterations = order.len()`) empêche les boucles infinies quand tous les blocs restants sont des tips.

### Bug 7 : Protection manquante dans RocksDB `remove_tip()` et `trim_tips()` (corrigé `89e1a2f`, 2026-03-11)

**Problème** : La protection du dernier tip avait été ajoutée dans `prune_oldest()` (RAM) mais pas dans `remove_tip()` et `trim_tips()` (RocksDB). En production, la CF `tips` pouvait se retrouver vide, causant `top_tips()` à retourner un vecteur vide. La [[fee-distribution|distribution des frais]] était bloquée pendant des heures sans aucune alerte visible.

**Correction** : `remove_tip()` compte les tips existants (`take(2).count()`) et refuse de supprimer le dernier. `trim_tips()` utilise `keep = tip_limit.max(1)` pour toujours garder au moins 1 tip. C'est le cas exemplaire du principe "dual-layer consistency" documenté dans `CLAUDE.md`.

### Bug 8 : Crash au bootstrap avec 971K blocs (corrigé `020e0a0`, 2026-03-12)

**Problème** : Sur testnet avec 971K blocs persistés, `bootstrap_from_store_with_capacity()` chargeait TOUS les blocs en RAM (O(n) lectures RocksDB + allocations) avant de pruner. Avec 97 agents et 2 semaines d'historique, cela prenait >10 minutes et 4+ GB de RAM, causant des crashes OOM sur les VPS 8 GB.

**Correction** : Chargement sélectif. `all_block_ids()` retourne les IDs triées en ordre lexicographique. Si le nombre d'IDs dépasse `max_blocks`, seuls les `max_blocks` derniers (lexicographiquement) sont chargés via `ids.split_off(skip)` (O(1)). Les blocs historiques restent dans RocksDB pour les requêtes API.

### Bug 9 : Métrique `pms_blocks_total` incorrecte (corrigé `28850b4`, 2026-02-17 et `c31b7dc`, 2026-02-18)

**Problème** : La gauge Prometheus `PMS_BLOCKS_TOTAL` était incrémentée à chaque `insert_block()` mais jamais décrémentée après pruning. Le dashboard affichait un nombre cumulatif croissant au lieu de la taille réelle du DAG en RAM.

**Correction** : Introduction de `sync_dag_size_metric()` qui lit `dag.len()` et met à jour la gauge avec un `set()`. Appelée à chaque fetch de `/metrics` pour refléter la taille réelle post-pruning.

### Bug 10 : Chargement lexicographique = 99.8% tips orphelins + OOM (corrigé v0.5.13, 2026-03-18)

**Problème** : Quatre causes combinées provoquaient un OOM après quelques heures sur le testnet (Docker `mem_limit: 7g`, Eden = 13.2M blocs) :

1. **`instance.rs` : `all_block_ids()` pour `is_empty()`** — chargeait les 13.2M IDs en `Vec<String>` (~1.16 GB) juste pour vérifier `.is_empty()`.
2. **Chargement lexicographique** — `newest_block_ids()` retournait les N IDs les plus grands lexicographiquement, mais les IDs sont des hashes → sélection aléatoire. 49,904/50,000 blocs chargés étaient des tips orphelins (parents hors fenêtre).
3. **Ghost entries** — Les ~49K parents référencés mais non chargés créaient des entries dans `children_count`/`children_idx` DashMaps (~100-800 MB).
4. **`block_count()` full scan** — `block_count()` scannait toute la CF `idx_blocks` (13.2M entries) juste pour un log diagnostique.

**Correction** :
- `is_empty()` : single iterator seek, O(1), 0 bytes.
- `newest_block_ids_by_time()` : reverse iterator sur `by_time` CF pour chargement chronologique. Fallback lexicographique si `by_time` vide.
- `cleanup_ghost_entries()` : supprime les entries DashMap pour les parents fantômes après Phase 1.
- `block_count_estimate()` : O(1) via `rocksdb.estimate-num-keys`.
- Résultat : pic mémoire réduit de ~6+ GB à ~3-4 GB, tips orphelins de 99.8% à ~2-5%.

### Bug 11 : Page cache OOM runtime + memory leaks (corrigé v0.5.16, 2026-03-19)

**Problème** : Même après les fixes bootstrap (v0.5.13) et `internal_health` (v0.5.15), l'engine crash toujours après plusieurs heures d'opération. Cinq causes combinées :

1. **Page cache RocksDB** — Sans `advise_random_on_open`, chaque lecture SST déclenchait 128 KB de readahead kernel. Avec 15M+ blocs et 65+ CFs (~15+ GB de SST sur disque), le page cache saturait la limite Docker de 7 GB.
2. **Activity cache** — L'éviction ne supprimait que les entries expirées (TTL). Si toutes étaient fraîches, rien n'était évincé même au-delà de `max_entries` (10,000). Croissance illimitée.
3. **Node registry** — `cleanup_stale()` défini mais jamais appelé. HashMap de nœuds croissait sans borne.
4. **`block_count()` full scan** — `main.rs` appelait `block_count()` (scan complet O(N)) au lieu de `block_count_estimate()` (O(1)).
5. **`utxo_store.rs` all_block_ids()** — Pour `scan_limit > 500`, chargeait TOUS les block IDs (~1.2 GB à 15M blocs) + tous les payloads en RAM.

**Correction** :
- `use_direct_io_for_flush_and_compaction(true)` → compaction/flush utilisent O_DIRECT (bypass page cache). Les lectures utilisateur conservent le readahead kernel. Note: `advise_random_on_open(true)` a été testé initialement mais causait un TPS regression 22x (2000→90) car il désactivait le readahead sur TOUTES les lectures SST.
- `compaction_readahead_size(2 MB)` → compaction garde un I/O séquentiel efficace avec direct I/O.
- Activity cache : éviction en 2 phases (expired, puis forcée à 70% si toujours plein).
- Node registry : `cleanup_stale()` appelé à chaque `register()`.
- `block_count_estimate()` partout, `recent_ids()` toujours borné.

## Interactions

### Distribution des frais (`fee_distribution/`)

La [[fee-distribution|distribution des frais]] dépend directement des tips :
- `perform_fee_distribution()` appelle `top_tips(1)` pour trouver un bloc parent où attacher le bloc de récompense.
- Si `top_tips()` retourne un vecteur vide (DAG sur-pruné ou CF tips vide), la distribution est **BLOQUÉE** et les frais s'accumulent dans le `FeePool`.
- Un message `tracing::error` est émis : `"Fee distribution BLOCKED: top_tips() returned empty. DAG may have been over-pruned."`.

Cette dépendance est la raison pour laquelle la protection du dernier tip est critique (Bugs 6 et 7).

### Bootstrap (`LedgerInstance::bootstrap`)

Au démarrage du nœud, chaque `LedgerInstance` appelle `ConcurrentDag::bootstrap_from_store_with_capacity()` avec les paramètres de la configuration `[rocks]`. La séquence est :

1. `is_empty()` — O(1) check si genesis nécessaire (remplace `all_block_ids()` qui allouait ~1.16 GB pour 13.2M blocs)
2. `newest_block_ids_by_time(max_blocks)` — chargement chronologique via `by_time` CF (fallback lexicographique)
3. `bootstrap_insert()` pour chaque bloc
4. `cleanup_ghost_entries()` — supprime les entries DashMap pour les parents hors fenêtre
5. Construction de `insertion_order` depuis l'ordre de chargement (oldest-first)
6. `prune_oldest()` unique post-chargement
7. Log : `"DAG loaded"` avec nombre de blocs + diagnostic (tips_count, orphan_parents)

### Métriques Prometheus

- **`pms_blocks_total`** (gauge, par [[multi-ledger|ledger]]) : Taille actuelle du DAG en RAM (`dag.len()`). Synchronisée à chaque fetch de `/metrics` et `/ledger/{id}/metrics`. Reflète le pruning.
- **`blocks_persisted`** (compteur, par [[multi-ledger|ledger]]) : Nombre total de blocs persistés dans RocksDB. Non affecté par le pruning RAM.

### Spent outpoints

Les `spent_outpoints` (DashSet) ne sont **jamais** prunés par `prune_oldest()`. Ils ont leur propre mécanisme de pruning FIFO via `mark_spent()` quand `max_spent_outpoints > 0`, utilisant une deque séparée (`spent_order`). Cela garantit que la détection de double-spend fonctionne même après que le bloc contenant la transaction a été évincé du RAM.

### Tips DashSet incrémental

Le `tips` DashSet est maintenu incrémentalement :
- `insert_block()` / `bootstrap_insert()` : ajoute le nouveau bloc comme tip, retire les parents du set
- `prune_oldest()` : retire les blocs prunés du set
- `find_tips()` : lit directement le set (O(tips) au lieu de O(all_blocks))

Cette maintenance incrémentale a remplacé un scan complet de `children_count` qui était O(n) et devenait un goulot d'étranglement avec 50K+ blocs.

### Finality

La `FinalityState` contient un `HashSet<BlockId>` des blocs finalisés. Sans pruning de ce set, il croîtrait indéfiniment (~340 MB économisés). `prune_oldest()` retire les IDs des blocs évictés de `finality.finalized` dans sa Phase 3.
