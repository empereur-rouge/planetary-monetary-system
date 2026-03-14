---
tags: [feature]
created: 2025-12-28
updated: 2026-03-14
version: v0.3.0
---

# Milestones & Finality (Checkpoints)

## Resume

Le systeme de Milestones & Finality est le mecanisme par lequel le DAG-PMS transforme un graphe acyclique dirige (DAG) en un registre a finalite deterministe. Il repose sur trois piliers :

1. **Milestones** : blocs speciaux emis exclusivement par le Coordinator qui approuvent un ensemble de blocs et declenchent optionnellement la distribution des recompenses aux noeuds.
2. **Finalite K-depth** : mecanisme automatique qui marque un bloc comme final des qu'il accumule au moins `k` descendants distincts (BFS).
3. **Checkpoints RocksDB** : snapshots physiques de la base de donnees avec rotation automatique pour la recuperation apres sinistre.

Ces trois composants cooperent pour garantir l'irreversibilite des transactions, la distribution equitable des frais, et la resilience du stockage.

---

## Mecanisme de Finalite

### Double finalite : Milestone + K-depth

Le DAG-PMS utilise deux mecanismes de finalite **complementaires** :

#### 1. Finalite par Milestone (explicite)

Le Coordinator emet periodiquement un bloc de type `PlainPayload::Milestone` contenant :

- `approved: Vec<String>` : liste des IDs de blocs approuves.
- `distribute_node_rewards: bool` : si `true`, declenche la distribution du pool de frais aux noeuds.

Lorsqu'un Milestone est insere dans le DAG :

1. Le `last_milestone` est mis a jour avec l'ID du bloc Milestone.
2. Un BFS (parcours en largeur) est lance depuis les blocs `approved` : chaque ancetre accessible est marque comme finalise dans `FinalityState.finalized`.
3. Le bloc Milestone lui-meme est finalise.

Ce mecanisme est **autoritaire** : seul le Coordinator (verifie par `coordinator_public_key`) peut emettre un Milestone. Toute tentative de Milestone signe par une autre cle est rejetee avec `ValidationError::InvalidSignature`.

```
Coordinator emet Milestone M
    |
    v
approved = [B1, B2, B3]
    |
    v
BFS depuis B1, B2, B3 -> marque final : B1, B2, B3, et tous leurs ancetres
    |
    v
M lui-meme est marque final
    |
    v
last_milestone = M.id (seed pour tips deterministes)
```

#### 2. Finalite K-depth (automatique, incrementale)

Independamment des Milestones, chaque bloc peut etre finalise automatiquement lorsqu'il a accumule `depth_k` descendants distincts. Ce seuil est configurable via `FinalityState::new(depth_k)`.

**Optimisation incrementale (v0.2.3+)** : au lieu de scanner tous les blocs du DAG a chaque insertion (O(N * BFS)), seuls les ancetres du bloc nouvellement insere sont verifies. La complexite passe de O(N * k) a O(k^2) par insertion.

```rust
// net_adapter.rs - Etape 6 : finality update
let ancestors = self.dag.ancestors_within_depth(&block.id, depth_k);
for ancestor_id in ancestors {
    if finality.finalized.contains(&ancestor_id) { continue; }
    let confirmations = self.dag.count_descendants(&ancestor_id, depth_k);
    if confirmations >= depth_k {
        finality.finalized.insert(ancestor_id.clone());
        newly_finalized.push(ancestor_id);
    }
}
```

### FinalityState

La structure centrale de l'etat de finalite :

```rust
pub struct FinalityState {
    pub finalized: HashSet<String>,       // IDs des blocs finalises
    pub last_milestone: Option<String>,   // dernier Milestone (seed pour tips)
    pub depth_k: usize,                  // seuil K-depth
}
```

**Fonctions cles :**

| Fonction | Description |
|---|---|
| `FinalityState::new(depth_k)` | Cree un etat de finalite avec le seuil K-depth |
| `FinalityState::is_final(id)` | Verifie si un bloc est finalise |
| `FinalityState::mark_final(id)` | Marque un bloc comme final |
| `FinalityState::set_milestone(id)` | Definit le dernier Milestone et le marque final |

### Seed deterministe pour la selection de tips

Le `last_milestone` sert de seed pour le tri deterministe des tips dans `select_parents_deterministic()` :

```rust
let seed = dag.finality.last_milestone.as_deref().unwrap_or("genesis");
tips.sort_by(|a, b| {
    let ha = Sha256::new().chain(seed).chain(a).finalize();
    let hb = Sha256::new().chain(seed).chain(b).finalize();
    ha.cmp(&hb)
});
```

Cela garantit que tous les noeuds selectionnent les memes parents de facon deterministe apres chaque Milestone.

### Persistance de la finalite

La finalite est persistee dans RocksDB via deux Column Families :

| Column Family | Cle | Valeur | Description |
|---|---|---|---|
| `final` | `block_id` (bytes) | `""` (vide) | Ensemble des blocs finalises |
| `last_ms` | `b"last"` | `milestone_id` (bytes) | ID du dernier Milestone |

**Persistance asynchrone** : les blocs nouvellement finalises sont envoyes au `background_persist_task` via un `PersistJob` contenant `newly_finalized: Vec<String>`. Le background task appelle `store.persist_final(&newly_finalized)` pour ecrire un `WriteBatch` atomique dans la CF `final`.

```rust
// background_persist.rs
if !job.newly_finalized.is_empty() {
    store.persist_final(&job.newly_finalized).await?;
}
```

Au redemarrage, `bootstrap_from_store()` recharge la finalite :

```rust
dag.finality.finalized.extend(store.load_final().await?);
dag.finality.last_milestone = store.load_last_milestone().await?;
```

### Pruning et finalite

Lors du pruning RAM (`prune_oldest()` dans `ConcurrentDag`), les entrees de `finalized` correspondant aux blocs supprimes sont nettoyees pour eviter une croissance memoire illimitee :

```rust
// concurrent_dag.rs - Phase 3 du pruning
match self.finality.write() {
    Ok(mut f) => {
        for old_id in &ids_to_remove {
            f.finalized.remove(old_id);
        }
    }
    // ...
}
```

---

## Distribution des recompenses via Milestone

Lorsqu'un Milestone est insere avec `distribute_node_rewards: true`, la logique suivante s'execute dans `net_adapter.rs` (etape 6) :

1. **Lecture du pool de frais** : `store.get_fee_pool()` retourne le total en satoshis.
2. **Lecture des compteurs** : `store.get_all_miners()` retourne `Vec<(node_pk, block_count)>`.
3. **Calcul proportionnel** : `share = pool * block_count / total_blocks` pour chaque noeud.
4. **Resolution d'adresse** : `store.get_node_reward_address(node_pk)` pour trouver l'adresse de recompense.
5. **Creation d'UTXOs** : les UTXOs de recompense sont crees en RAM puis ajoutes au ShardedUtxoSet.
6. **Reset** : `store.reset_pool_and_counts()` remet le pool et les compteurs a zero.
7. **Evenement** : `PmsEvent::NodeRewardDistributed` est emis pour chaque noeud recompense.

### Distribution automatique (Timer)

En parallele du mecanisme Milestone, un timer automatique distribue les frais periodiquement :

- Configure via `settings.fees.distribution_interval_sec` (defaut : 600s = 10 minutes).
- Appelle `perform_fee_distribution()` qui cree un bloc `PlainPayload::Mint` contenant les UTXOs de distribution.
- Inclut : burn refunds, treasury tax, et parts proportionnelles des noeuds.

### Endpoint API

| Methode | Route | Description |
|---|---|---|
| `POST` | `/admin/distribute_fees` | Declenche manuellement la distribution (Coordinator uniquement) |
| `GET` | `/v1/fee_pool` | Etat du pool de frais (public) |

---

## Checkpoints RocksDB (snapshots, rotation)

### Creation de checkpoints

La methode `RocksStore::create_checkpoint(backup_root)` cree un snapshot atomique de la base RocksDB :

1. Cree le dossier `backup_root` si inexistant.
2. Genere un nom unique : `rocks-YYYYMMDD-HHMMSS` (ex: `rocks-20260314-153042`).
3. Appelle `rocksdb::Checkpoint::create_checkpoint()` qui cree un snapshot coherent sans interrompre les ecritures.

```rust
// rocks_store/helpers.rs
pub fn create_checkpoint(&self, backup_root: &str) -> Result<()> {
    let ts = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let dest = root.join(format!("rocks-{ts}"));
    let cp = Checkpoint::new(&*self.db)?;
    cp.create_checkpoint(&dest)?;
}
```

### Rotation des checkpoints

La fonction `rotate_checkpoints(backup_root, keep_last)` gere la retention :

1. Liste tous les dossiers `rocks-*` dans `backup_root`.
2. Tri lexicographique (= chronologique grace au format `YYYYMMDD-HHMMSS`).
3. Supprime les plus anciens pour ne garder que `keep_last` snapshots.
4. Si `keep_last == 0`, supprime tout.

```rust
// checkpoint_rocks.rs
pub fn rotate_checkpoints(backup_root: &str, keep_last: usize) -> Result<()>
```

### Test d'integration

Le test `rocks_checkpoints_are_created_and_rotated` (`rocks_checkpoints.rs`) valide le cycle complet :

1. Cree un `RocksStore` temporaire.
2. Genere 4 checkpoints espaces de 1.1s.
3. Verifie qu'au moins 4 dossiers `rocks-*` existent.
4. Appelle `rotate_checkpoints(_, keep_last=2)`.
5. Verifie qu'il reste exactement 2 checkpoints (les plus recents).

---

## Crates et Fichiers

### Crate `pms-core`

| Fichier | Description |
|---|---|
| `crates/pms-core/src/finality.rs` | `FinalityState`, `has_k_confirmations_dag()` - etat et logique de finalite |
| `crates/pms-core/src/dag.rs` | `Dag` - DAG legacy avec `maybe_update_finality_with()`, `update_finality_after_insert()` |
| `crates/pms-core/src/concurrent_dag.rs` | `ConcurrentDag` - DAG concurrent (DashMap) avec `count_descendants()`, `ancestors_within_depth()` |
| `crates/pms-core/src/net_adapter.rs` | `persist_block()` - pipeline d'insertion avec finalite Milestone + K-depth |
| `crates/pms-core/src/background_persist.rs` | `PersistJob` - persistance asynchrone incluant `newly_finalized` |
| `crates/pms-core/src/tips.rs` | `select_parents_deterministic()` - selection de tips basee sur `last_milestone` |
| `crates/pms-core/src/validations/check.rs` | Validation des Milestones (signature Coordinator obligatoire) |
| `crates/pms-core/src/validations/apply.rs` | `apply_block_mem()` - application RAM avec `update_finality_after_insert()` |

### Crate `pms-types-payload`

| Fichier | Description |
|---|---|
| `crates/pms-types-payload/src/payload.rs` | `PlainPayload::Milestone { approved, distribute_node_rewards }` |

### Crate `pms-storage`

| Fichier | Description |
|---|---|
| `crates/pms-storage/src/traits.rs` | Trait `DagStorage` avec `persist_final()`, `load_final()`, `load_last_milestone()`, `persist_last_milestone()` |
| `crates/pms-storage/src/rocks_store/store.rs` | Implementation RocksDB : CFs `final` et `last_ms` |
| `crates/pms-storage/src/rocks_store/helpers.rs` | `RocksStore::create_checkpoint()` - snapshots RocksDB |
| `crates/pms-storage/src/rocks_store/node_rewards_storage.rs` | `NodeRewardsStorage` : pool de fees, compteurs de blocs par noeud, adresses de recompense |
| `crates/pms-storage/src/checkpoint_rocks.rs` | `rotate_checkpoints()` - rotation des snapshots |
| `crates/pms-storage/tests/rocks_checkpoints.rs` | Test d'integration checkpoints + rotation |

### Crate `pms-server`

| Fichier | Description |
|---|---|
| `crates/pms-server/src/api_fn/milestone.rs` | `distribute_fees()` (POST /admin/distribute_fees), `get_fee_pool_status()` (GET /v1/fee_pool) |
| `crates/pms-server/src/fee_pool.rs` | `FeePool` - accumulation des frais, calcul des parts proportionnelles |
| `crates/pms-server/src/fee_distribution.rs` | `perform_fee_distribution()`, `perform_daily_inflation_mint()` |
| `crates/pms-server/src/api.rs` | `spawn_fee_distributor_task()` - timer automatique de distribution |

### Crate `pms-event`

| Fichier | Description |
|---|---|
| `crates/pms-event/src/events.rs` | `PmsEvent::MilestoneConfirmed`, `PmsEvent::NodeRewardDistributed` |

---

## Fonctions Cles

### Finalite

| Fonction | Fichier | Signature |
|---|---|---|
| `FinalityState::new` | `finality.rs` | `fn new(depth_k: usize) -> Self` |
| `FinalityState::is_final` | `finality.rs` | `fn is_final(&self, id: &str) -> bool` |
| `FinalityState::mark_final` | `finality.rs` | `fn mark_final(&mut self, id: &str)` |
| `FinalityState::set_milestone` | `finality.rs` | `fn set_milestone(&mut self, id: String)` |
| `has_k_confirmations_dag` | `finality.rs` | `fn has_k_confirmations_dag(dag: &Dag, b: &str, k: usize) -> bool` |
| `Dag::maybe_update_finality_with` | `dag.rs` | `fn maybe_update_finality_with(&mut self, block: &Block)` |
| `Dag::update_finality_after_insert` | `dag.rs` | `fn update_finality_after_insert(&mut self, new_block_id: &str)` |
| `Dag::is_final` | `dag.rs` | `fn is_final(&self, id: &str) -> bool` |
| `ConcurrentDag::count_descendants` | `concurrent_dag.rs` | `fn count_descendants(&self, id: &str, max_count: usize) -> usize` |
| `ConcurrentDag::ancestors_within_depth` | `concurrent_dag.rs` | `fn ancestors_within_depth(&self, id: &str, max_depth: usize) -> Vec<BlockId>` |
| `ConcurrentDag::is_final` | `concurrent_dag.rs` | `fn is_final(&self, block_id: &str) -> bool` |

### Persistance

| Fonction | Fichier | Signature |
|---|---|---|
| `DagStorage::persist_final` | `traits.rs` | `async fn persist_final(&self, ids: &[String]) -> Result<()>` |
| `DagStorage::load_final` | `traits.rs` | `async fn load_final(&self) -> Result<Vec<String>>` |
| `DagStorage::load_last_milestone` | `traits.rs` | `async fn load_last_milestone(&self) -> Result<Option<String>>` |
| `DagStorage::persist_last_milestone` | `traits.rs` | `async fn persist_last_milestone(&self, id: &str) -> Result<()>` |
| `spawn_background_persist` | `background_persist.rs` | `fn spawn_background_persist<S>(store, buffer_size) -> (Sender, JoinHandle)` |

### Checkpoints

| Fonction | Fichier | Signature |
|---|---|---|
| `RocksStore::create_checkpoint` | `helpers.rs` | `fn create_checkpoint(&self, backup_root: &str) -> Result<()>` |
| `rotate_checkpoints` | `checkpoint_rocks.rs` | `fn rotate_checkpoints(backup_root: &str, keep_last: usize) -> Result<()>` |

### Distribution

| Fonction | Fichier | Signature |
|---|---|---|
| `perform_fee_distribution` | `fee_distribution.rs` | `async fn perform_fee_distribution(state: &AppState, parent_id: Option<String>) -> Result<DistributeFeesResult>` |
| `FeePool::add_fee` | `fee_pool.rs` | `fn add_fee(&mut self, fee: Decimal, node_pk: &str)` |
| `FeePool::calculate_shares` | `fee_pool.rs` | `fn calculate_shares(&self) -> Vec<(String, Decimal, Decimal)>` |
| `spawn_fee_distributor_task` | `api.rs` | `fn spawn_fee_distributor_task(state: AppState)` |
| `distribute_fees` | `milestone.rs` | `async fn distribute_fees(State, Json) -> impl IntoResponse` |
| `NodeRewardsStorage::increment_node_block_count` | `node_rewards_storage.rs` | `fn increment_node_block_count(&self, node_pk: &str) -> Result<()>` |
| `NodeRewardsStorage::get_all_miners` | `node_rewards_storage.rs` | `fn get_all_miners(&self) -> Result<Vec<(String, u64)>>` |
| `NodeRewardsStorage::reset_pool_and_counts` | `node_rewards_storage.rs` | `fn reset_pool_and_counts(&self) -> Result<()>` |

---

## Interactions

### [[node-rewards]]

Le systeme de Milestones est le declencheur principal de la distribution des recompenses :

- Chaque bloc insere incremente le compteur du noeud signataire via `increment_node_block_count()` dans la CF `node_block_counts`.
- Les frais de transaction sont accumules dans la CF `node_fee_pool`.
- Un Milestone avec `distribute_node_rewards: true` declenche la distribution proportionnelle du pool aux noeuds.
- L'evenement `PmsEvent::NodeRewardDistributed` est emis pour chaque noeud recompense, contenant `milestone_id`, `node_pk`, `address`, et `amount_sats`.

### [[event-system]]

Le systeme de Milestones emet deux types d'evenements via l'`EventBus` :

- `PmsEvent::MilestoneConfirmed { block_id, approved_blocks }` : emis quand un Milestone est confirme.
- `PmsEvent::NodeRewardDistributed { node_pk, address, amount_sats, milestone_id }` : emis pour chaque recompense distribuee.

Ces evenements sont consommes par le flux SSE (`/v1/wallet/{address}/activity/stream`) pour notification temps reel.

### [[storage-rocksdb]]

Le systeme de finalite utilise 4 Column Families RocksDB :

| CF | Usage |
|---|---|
| `final` | Ensemble des blocs finalises (cle = block_id, valeur = vide) |
| `last_ms` | Dernier Milestone (cle fixe `"last"`, valeur = milestone_id) |
| `node_block_counts` | Compteurs de blocs par noeud (cle = node_pk, valeur = u64 LE) |
| `node_fee_pool` | Pool de frais (cle `"pool"` = u64 LE, cle `"total_burned"` = Decimal string) |

Le trait `DagStorage` definit l'interface de persistance de la finalite. L'implementation `RocksStore` utilise des `WriteBatch` atomiques pour garantir la coherence.

Les checkpoints RocksDB (`create_checkpoint()`) creent des snapshots physiques complets incluant toutes les CFs, y compris `final` et `last_ms`. La rotation (`rotate_checkpoints()`) ne conserve que les `keep_last` snapshots les plus recents.

### Flux complet d'un Milestone

```
1. Coordinator POST /admin/distribute_fees
       |
       v
2. perform_fee_distribution() cree un bloc Mint
       |
       v
3. persist_block() dans net_adapter.rs
       |
       v
4. Etape 6 : Detection PlainPayload::Milestone
       |
       +-- set last_milestone
       +-- BFS depuis approved -> mark final
       +-- si distribute_node_rewards:
       |       +-- get_fee_pool()
       |       +-- get_all_miners()
       |       +-- calcul proportionnel
       |       +-- creation UTXOs
       |       +-- reset_pool_and_counts()
       |       +-- emit NodeRewardDistributed
       |
       v
5. PersistJob { newly_finalized } -> background task
       |
       v
6. store.persist_final() -> WriteBatch sur CF "final"
```
