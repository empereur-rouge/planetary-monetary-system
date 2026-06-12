use dashmap::{DashMap, DashSet};
use lru::LruCache;
use parking_lot::Mutex;
use pms_types::{OutputId, TxOutput};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Nombre de shards (256 = 1 octet du hash)
const SHARD_COUNT: usize = 256;

/// Horloge du protocole : timestamp UNIX courant en millisecondes.
/// Source de temps unique pour les règles temporelles (time-lock 2.1,
/// demurrage 2.5) — même pattern que `now_ms_for_signers` dans `persist.rs`.
pub fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Cache du supply par asset_id : (total, count).
type SupplyCache = HashMap<Option<String>, (Decimal, usize)>;

/// Fallback closure pour récupérer un UTXO depuis RocksDB quand il est absent du cache LRU.
/// Signature : (txid, index) -> Option<TxOutput>
pub type UtxoFetcher = Arc<dyn Fn(&str, u32) -> Option<TxOutput> + Send + Sync>;

// ─── Compact internal representation ───────────────────────────────

/// Representation compacte d'un TxOutput en RAM.
/// ~32 bytes (stack) vs ~148 bytes (stack + heap) par TxOutput.
///
/// - `Arc<str>` pour l'adresse : interné, partagé entre tous les UTXOs d'une même adresse
/// - `Decimal` (16 bytes stack) au lieu de `String` pour le montant
/// - `Option<Arc<str>>` pour l'asset_id : interné, None = PMS natif (0 heap)
struct CompactOutput {
    address: Arc<str>,
    amount: Decimal,
    asset_id: Option<Arc<str>>,
    /// Time-lock (timestamp UNIX ms). DOIT être propagé depuis `TxOutput` —
    /// un lock perdu ici devient invisible au validateur (piège check-list
    /// « Cache UTXO RAM »).
    locked_until: Option<u64>,
    /// Condition de déverrouillage (protocole 2.2). Boxée + partagée en Arc :
    /// rare en pratique (None = 8 bytes), et le clone lors de l'éviction LRU /
    /// re-push reste O(1).
    spend_condition: Option<Arc<pms_types::SpendCondition>>,
}

impl CompactOutput {
    fn to_tx_output(&self) -> TxOutput {
        TxOutput {
            address: self.address.to_string(),
            amount: self.amount.to_string(),
            asset_id: self.asset_id.as_ref().map(|a| a.to_string()),
            locked_until: self.locked_until,
            spend_condition: self.spend_condition.as_deref().cloned(),
        }
    }
}

/// Interner pour dédupliquer les chaînes identiques via `Arc<str>`.
/// Seul le coordinateur écrit, donc la contention Mutex est quasi nulle.
struct Interner {
    set: Mutex<HashSet<Arc<str>>>,
}

impl Interner {
    fn new() -> Self {
        Self {
            set: Mutex::new(HashSet::new()),
        }
    }

    fn intern(&self, s: &str) -> Arc<str> {
        let mut set = self.set.lock();
        if let Some(existing) = set.get(s) {
            existing.clone()
        } else {
            let arc: Arc<str> = Arc::from(s);
            set.insert(arc.clone());
            arc
        }
    }
}

// ─── ShardedUtxoSet ────────────────────────────────────────────────

/// UTXO Set partitionné en RAM pour accès concurrent avec cache LRU borné.
///
/// Remplace le gros `Dag` lock pour la validation des transactions.
/// Chaque shard est protégé par un RwLock indépendant et borné par un LruCache.
///
/// Quand un UTXO est évincé du cache LRU, il reste dans RocksDB.
/// Les lectures en cache miss utilisent le `fallback` (closure) pour récupérer
/// l'UTXO depuis RocksDB.
///
/// Les caches secondaires (supply_cache, native_balance_cache, address_index)
/// restent **toujours exacts** et ne sont pas affectés par l'éviction LRU.
///
/// Stocke en interne des `CompactOutput` (~32 bytes) au lieu de `TxOutput`
/// (~148 bytes) grâce à l'interning des adresses et Decimal stack-allocated.
pub struct ShardedUtxoSet {
    shards: [RwLock<LruCache<OutputId, CompactOutput>>; SHARD_COUNT],
    /// Index secondaire : address -> set of OutputIds
    address_index: DashMap<String, DashSet<OutputId>>,
    /// Cache supply incrémental : asset_id -> (total_amount, utxo_count)
    supply_cache: Mutex<SupplyCache>,
    /// Per-address native (PMS) balance cache — avoids shard read lock starvation
    /// under continuous write load (e.g. heavy token minting).
    native_balance_cache: DashMap<String, Decimal>,
    /// Per-address per-asset token balance cache — O(1) lookup for custom tokens.
    /// Mirrors `native_balance_cache` but keyed by `(address, asset_id)`.
    /// Uses interned `Arc<str>` keys to avoid String allocation per lookup.
    /// Zero-balance entries are removed to prevent unbounded growth.
    /// Memory: ~80 bytes per entry. 100K unique (addr, asset) pairs ≈ 8 MB.
    token_balance_cache: DashMap<(Arc<str>, Arc<str>), Decimal>,
    /// Interner pour dédupliquer addresses et asset_ids
    interner: Interner,
    /// Fallback vers RocksDB pour les cache misses
    fallback: Option<UtxoFetcher>,
}

impl ShardedUtxoSet {
    /// Crée un nouveau ShardedUtxoSet avec cache LRU borné.
    ///
    /// - `max_utxos`: capacité maximale totale en RAM (répartie sur 256 shards).
    ///   `0` = illimité (pas d'éviction).
    /// - `fallback`: closure pour récupérer un UTXO depuis RocksDB en cas de cache miss.
    pub fn new(max_utxos: usize, fallback: Option<UtxoFetcher>) -> Self {
        let shards = if max_utxos == 0 {
            // Unbounded: no eviction (backward-compatible)
            std::array::from_fn(|_| RwLock::new(LruCache::unbounded()))
        } else {
            // At least 1 per shard, ceiling division for even distribution
            let cap_per_shard =
                NonZeroUsize::new(((max_utxos + SHARD_COUNT - 1) / SHARD_COUNT).max(1)).unwrap();
            std::array::from_fn(|_| RwLock::new(LruCache::new(cap_per_shard)))
        };
        Self {
            shards,
            address_index: DashMap::new(),
            supply_cache: Mutex::new(SupplyCache::new()),
            native_balance_cache: DashMap::new(),
            token_balance_cache: DashMap::new(),
            interner: Interner::new(),
            fallback,
        }
    }

    /// Détermine l'index du shard (0-255) basé sur le premier octet du TxId.
    fn shard_index(out_point: &OutputId) -> usize {
        let s = &out_point.txid;
        if s.len() >= 2 {
            u8::from_str_radix(&s[0..2], 16).unwrap_or(0) as usize
        } else {
            0
        }
    }

    /// Convertit un TxOutput en CompactOutput avec interning.
    fn compact(&self, output: &TxOutput) -> CompactOutput {
        let amount = Decimal::from_str(&output.amount).unwrap_or(Decimal::ZERO);
        CompactOutput {
            address: self.interner.intern(&output.address),
            amount,
            asset_id: output.asset_id.as_deref().map(|a| self.interner.intern(a)),
            locked_until: output.locked_until,
            spend_condition: output.spend_condition.clone().map(Arc::new),
        }
    }

    /// Récupère un UTXO depuis le fallback RocksDB (si configuré).
    fn fetch_from_store(&self, out_point: &OutputId) -> Option<TxOutput> {
        self.fallback
            .as_ref()
            .and_then(|f| f(&out_point.txid, out_point.index))
    }

    // ─── Supply cache helpers (private) ──────────────────────────────

    fn supply_add_compact(&self, output: &CompactOutput) {
        let mut cache = self.supply_cache.lock();
        let key = output.asset_id.as_ref().map(|a| a.to_string());
        let entry = cache.entry(key).or_insert((Decimal::ZERO, 0));
        entry.0 += output.amount;
        entry.1 += 1;
        drop(cache);

        // Maintain native balance cache (lock-free DashMap, no shard dependency)
        if output.asset_id.is_none() {
            self.native_balance_cache
                .entry(output.address.to_string())
                .and_modify(|b| *b += output.amount)
                .or_insert(output.amount);
        }

        // Maintain token balance cache (lock-free DashMap, O(1) per lookup)
        if let Some(ref asset_arc) = output.asset_id {
            self.token_balance_cache
                .entry((output.address.clone(), asset_arc.clone()))
                .and_modify(|b| *b += output.amount)
                .or_insert(output.amount);
        }
    }

    fn supply_sub_compact(&self, output: &CompactOutput) {
        let mut cache = self.supply_cache.lock();
        let key = output.asset_id.as_ref().map(|a| a.to_string());
        if let Some(entry) = cache.get_mut(&key) {
            entry.0 -= output.amount;
            entry.1 = entry.1.saturating_sub(1);
            if entry.1 == 0 {
                cache.remove(&key);
            }
        }
        drop(cache);

        // Maintain native balance cache — remove zero-balance entries to prevent leak
        if output.asset_id.is_none() {
            if let Some(mut entry) = self.native_balance_cache.get_mut(&*output.address) {
                *entry.value_mut() -= output.amount;
                if entry.value().is_zero() {
                    let addr = entry.key().clone();
                    drop(entry);
                    self.native_balance_cache.remove(&addr);
                }
            }
        }

        // Maintain token balance cache — remove zero-balance entries to prevent leak
        if let Some(ref asset_arc) = output.asset_id {
            let key = (output.address.clone(), asset_arc.clone());
            if let Some(mut entry) = self.token_balance_cache.get_mut(&key) {
                *entry.value_mut() -= output.amount;
                if entry.value().is_zero() {
                    let k = entry.key().clone();
                    drop(entry);
                    self.token_balance_cache.remove(&k);
                }
            }
        }
    }

    /// Met à jour supply/balance via un TxOutput (utilisé pour les fallback RocksDB
    /// quand le CompactOutput n'est plus en cache LRU).
    fn supply_sub_txoutput(&self, output: &TxOutput) {
        let compact = self.compact(output);
        self.supply_sub_compact(&compact);
    }

    // ─── Address index helpers (private) ─────────────────────────────

    fn addr_index_add(&self, address: &str, out_point: &OutputId) {
        self.address_index
            .entry(address.to_string())
            .or_insert_with(DashSet::new)
            .insert(out_point.clone());
    }

    fn addr_index_remove(&self, address: &str, out_point: &OutputId) {
        if let Some(set) = self.address_index.get(address) {
            set.remove(out_point);
            if set.is_empty() {
                drop(set);
                self.address_index.remove(address);
            }
        }
    }

    // ─── Core UTXO operations ────────────────────────────────────────

    /// Récupère un UTXO (lecture concurrente via peek — pas de mutation LRU).
    /// En cas de cache miss, fallback vers RocksDB sans refill du cache
    /// (pour garder le read lock et éviter la contention write).
    pub async fn get(&self, out_point: &OutputId) -> Option<TxOutput> {
        let idx = Self::shard_index(out_point);
        {
            let shard = self.shards[idx].read().await;
            if let Some(c) = shard.peek(out_point) {
                return Some(c.to_tx_output());
            }
        }
        // Cache miss → fallback RocksDB (pas de cache refill pour garder le read lock)
        self.fetch_from_store(out_point)
    }

    /// Ajoute un UTXO (écriture ciblée).
    /// Supply/balance caches restent exacts (non affectés par l'éviction LRU).
    /// L'`address_index` est nettoyé pour les entrées évincées du LRU.
    pub async fn add(&self, out_point: OutputId, output: TxOutput) {
        let compact = self.compact(&output);
        let address = compact.address.to_string();
        self.supply_add_compact(&compact);
        let idx = Self::shard_index(&out_point);
        let evicted = {
            let mut shard = self.shards[idx].write().await;
            shard.push(out_point.clone(), compact)
        }; // shard write lock dropped before touching DashMap
        self.addr_index_add(&address, &out_point);
        // Clean up address_index for LRU-evicted entry (prevents unbounded growth)
        if let Some((evicted_id, evicted_compact)) = evicted {
            self.addr_index_remove(&evicted_compact.address, &evicted_id);
        }
    }

    /// Supprime un UTXO (spend).
    /// Si l'UTXO a été évincé du cache LRU, utilise le fallback RocksDB
    /// pour récupérer les données nécessaires aux mises à jour supply/balance.
    pub async fn remove(&self, out_point: &OutputId) -> Option<TxOutput> {
        let idx = Self::shard_index(out_point);
        let removed = {
            let mut shard = self.shards[idx].write().await;
            shard.pop(out_point)
        }; // shard write lock dropped here before touching DashMap

        if let Some(ref compact) = removed {
            // Found in LRU cache — update indexes normally
            self.addr_index_remove(&compact.address, out_point);
            self.supply_sub_compact(compact);
            return removed.map(|c| c.to_tx_output());
        }

        // Cache miss → fallback to RocksDB for supply/balance updates
        if let Some(txo) = self.fetch_from_store(out_point) {
            self.addr_index_remove(&txo.address, out_point);
            self.supply_sub_txoutput(&txo);
            return Some(txo);
        }

        None
    }

    /// Applique un delta complet (spend + create) de manière concurrente.
    /// Groups operations by shard to minimize lock acquisitions.
    /// DashMap index updates are deferred until AFTER shard locks are released
    /// to prevent deadlock with balance/utxo queries (which take DashMap → shard).
    ///
    /// Les spends en cache miss utilisent le fallback RocksDB pour les mises à jour
    /// supply/balance.
    pub async fn apply_diff(&self, spends: &[OutputId], creates: &[(OutputId, TxOutput)]) {
        use std::collections::HashMap as StdHashMap;

        // Pre-compact all creates
        let compact_creates: Vec<(OutputId, CompactOutput)> = creates
            .iter()
            .map(|(id, out)| (id.clone(), self.compact(out)))
            .collect();

        // Group spends by shard index
        let mut spend_by_shard: StdHashMap<usize, Vec<&OutputId>> = StdHashMap::new();
        for sp in spends {
            spend_by_shard
                .entry(Self::shard_index(sp))
                .or_default()
                .push(sp);
        }

        // Group creates by shard index
        let mut create_by_shard: StdHashMap<usize, Vec<&(OutputId, CompactOutput)>> =
            StdHashMap::new();
        for item in &compact_creates {
            create_by_shard
                .entry(Self::shard_index(&item.0))
                .or_default()
                .push(item);
        }

        // Collect all affected shard indices
        let mut affected: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        affected.extend(spend_by_shard.keys());
        affected.extend(create_by_shard.keys());

        // Deferred DashMap index updates: (address, outpoint, is_add)
        // Collected while holding shard lock, applied after releasing it.
        let mut deferred_index_ops: Vec<(String, OutputId, bool)> = Vec::new();

        // Spends that missed the LRU cache — need fallback after shard lock is released
        let mut missed_spends: Vec<OutputId> = Vec::new();

        // LRU-evicted entries that need address_index cleanup (after lock release)
        let mut evicted_entries: Vec<(OutputId, CompactOutput)> = Vec::new();

        // Process each shard with a single write lock
        for shard_idx in affected {
            {
                let mut shard = self.shards[shard_idx].write().await;

                // Spends: lookup address before removal, defer index update
                if let Some(sp_list) = spend_by_shard.get(&shard_idx) {
                    for sp in sp_list {
                        if let Some(compact) = shard.pop(*sp) {
                            deferred_index_ops.push((
                                compact.address.to_string(),
                                (*sp).clone(),
                                false, // remove
                            ));
                            self.supply_sub_compact(&compact);
                        } else {
                            // Cache miss — defer to fallback after lock release
                            missed_spends.push((*sp).clone());
                        }
                    }
                }

                // Creates: insert into shard, defer index update
                // Capture LRU-evicted entries for address_index cleanup
                if let Some(cr_list) = create_by_shard.get(&shard_idx) {
                    for (id, compact) in cr_list.iter().map(|item| (&item.0, &item.1)) {
                        deferred_index_ops.push((
                            compact.address.to_string(),
                            id.clone(),
                            true, // add
                        ));
                        self.supply_add_compact(compact);
                        if let Some(evicted) = shard.push(
                            id.clone(),
                            CompactOutput {
                                address: compact.address.clone(),
                                amount: compact.amount,
                                asset_id: compact.asset_id.clone(),
                                locked_until: compact.locked_until,
                                spend_condition: compact.spend_condition.clone(),
                            },
                        ) {
                            evicted_entries.push(evicted);
                        }
                    }
                }
            } // shard write lock dropped here
        }

        // Handle missed spends via fallback (no shard lock held)
        for sp in &missed_spends {
            if let Some(txo) = self.fetch_from_store(sp) {
                deferred_index_ops.push((txo.address.clone(), sp.clone(), false));
                self.supply_sub_txoutput(&txo);
            }
        }

        // Apply deferred DashMap index updates (no shard lock held)
        for (address, outpoint, is_add) in deferred_index_ops {
            if is_add {
                self.addr_index_add(&address, &outpoint);
            } else {
                self.addr_index_remove(&address, &outpoint);
            }
        }

        // Clean up address_index for LRU-evicted entries (prevents unbounded growth)
        for (evicted_id, evicted_compact) in evicted_entries {
            self.addr_index_remove(&evicted_compact.address, &evicted_id);
        }
    }

    /// Taille totale en cache (somme des lens des shards LRU) - lent car lock tout
    pub async fn total_len(&self) -> usize {
        let mut total = 0;
        for s in &self.shards {
            total += s.read().await.len();
        }
        total
    }

    // ─── Supply queries (cached) ─────────────────────────────────────

    /// Retourne le supply PMS natif en circulation depuis le cache.
    pub async fn circulating_supply(&self) -> (Decimal, usize) {
        let cache = self.supply_cache.lock();
        cache.get(&None).cloned().unwrap_or((Decimal::ZERO, 0))
    }

    /// Retourne le supply d'un token spécifique depuis le cache.
    pub async fn circulating_supply_by_asset(&self, asset_id: Option<&str>) -> (Decimal, usize) {
        let cache = self.supply_cache.lock();
        let key = asset_id.map(|s| s.to_string());
        cache.get(&key).cloned().unwrap_or((Decimal::ZERO, 0))
    }

    // ─── Balance queries (indexed) ───────────────────────────────────

    /// Calcule la balance PMS native d'une adresse via le cache (O(1), pas de shard lock).
    pub async fn balance_by_address(&self, address: &str) -> Decimal {
        self.native_balance_cache
            .get(address)
            .map(|r| *r.value())
            .unwrap_or(Decimal::ZERO)
    }

    /// Calcule la balance d'une adresse pour un asset spécifique.
    /// Native (None) et Token (Some): lecture directe du cache O(1), pas de shard lock.
    pub async fn balance_by_address_and_asset(
        &self,
        address: &str,
        asset_id: Option<&str>,
    ) -> Decimal {
        // Native PMS: use the lock-free native balance cache
        if asset_id.is_none() {
            return self.balance_by_address(address).await;
        }

        // Token balance: O(1) via token_balance_cache (no shard lock, no RocksDB fallback)
        let asset = asset_id.unwrap(); // safe: None case handled above
        let addr_arc = self.interner.intern(address);
        let asset_arc = self.interner.intern(asset);
        self.token_balance_cache
            .get(&(addr_arc, asset_arc))
            .map(|r| *r.value())
            .unwrap_or(Decimal::ZERO)
    }

    /// Retourne tous les UTXOs d'une adresse (tous les assets).
    /// Utilise l'index secondaire. En cas de cache miss LRU, fallback vers RocksDB.
    ///
    /// Groups lookups by shard to minimize lock acquisitions:
    /// max 256 locks instead of N (one per UTXO).
    pub async fn utxos_by_address(&self, address: &str) -> Vec<(OutputId, TxOutput)> {
        // Collect OutputIds first, then DROP the DashMap guard before awaiting shard locks.
        // This prevents deadlock with apply_diff() which holds shard write lock → DashMap.
        let out_points: Vec<OutputId> = match self.address_index.get(address) {
            Some(utxo_ids) => utxo_ids.iter().map(|r| r.key().clone()).collect(),
            None => return Vec::new(),
        };
        // DashMap Ref guard is dropped here

        // Group by shard index to acquire each lock only once
        let mut by_shard: HashMap<usize, Vec<OutputId>> = HashMap::new();
        for op in out_points {
            by_shard.entry(Self::shard_index(&op)).or_default().push(op);
        }

        let mut result = Vec::new();
        let mut fallback_needed: Vec<OutputId> = Vec::new();

        for (shard_idx, ops) in by_shard {
            let shard = self.shards[shard_idx].read().await;
            for op in ops {
                if let Some(compact) = shard.peek(&op) {
                    result.push((op, compact.to_tx_output()));
                } else {
                    // Cache miss — collect for fallback after releasing shard lock
                    fallback_needed.push(op);
                }
            }
        }

        // Fetch missed UTXOs from RocksDB
        for op in fallback_needed {
            if let Some(txo) = self.fetch_from_store(&op) {
                result.push((op, txo));
            }
        }

        result
    }

    /// Returns up to `limit` UTXOs for an address matching `asset_id`,
    /// stopping early once enough value is accumulated.
    ///
    /// This avoids cloning ALL OutputIds from the DashSet for addresses
    /// with millions of UTXOs (e.g., coordinator receiving fee rewards).
    /// The `limit * 4` over-collection handles asset_id filtering mismatches.
    ///
    /// Returns `(selected_utxos_with_amounts, total_amount)`.
    pub async fn utxos_by_address_for_selection(
        &self,
        address: &str,
        asset_id: &Option<String>,
        target: Decimal,
        limit: usize,
    ) -> (Vec<(OutputId, TxOutput, Decimal)>, Decimal) {
        // Collect OutputIds with early exit — at most limit*4 to handle asset filtering
        let collect_cap = limit.saturating_mul(4).max(256);
        let out_points: Vec<OutputId> = match self.address_index.get(address) {
            Some(utxo_ids) => {
                let mut collected = Vec::with_capacity(collect_cap.min(utxo_ids.len()));
                for r in utxo_ids.iter() {
                    collected.push(r.key().clone());
                    if collected.len() >= collect_cap {
                        break;
                    }
                }
                collected
            }
            None => return (Vec::new(), Decimal::ZERO),
        };
        // DashMap Ref guard is dropped here

        // Group by shard index to acquire each lock only once
        let mut by_shard: HashMap<usize, Vec<OutputId>> = HashMap::new();
        for op in out_points {
            by_shard.entry(Self::shard_index(&op)).or_default().push(op);
        }

        let mut result = Vec::new();
        let mut total = Decimal::ZERO;
        let mut fallback_needed: Vec<OutputId> = Vec::new();

        // Les UTXOs time-lockés encore verrouillés sont exclus de la sélection :
        // le validateur hot path les rejetterait (OutputTimeLocked).
        let now_ms = current_time_ms();

        for (shard_idx, ops) in &by_shard {
            let shard = self.shards[*shard_idx].read().await;
            for op in ops {
                if let Some(compact) = shard.peek(op) {
                    let matches = match (&compact.asset_id, asset_id) {
                        (None, None) => true,
                        (Some(a), Some(b)) => a.as_ref() == b.as_str(),
                        _ => false,
                    } && compact.locked_until.is_none_or(|l| l <= now_ms);
                    if matches {
                        result.push((op.clone(), compact.to_tx_output(), compact.amount));
                        total += compact.amount;
                        if result.len() >= limit && total >= target {
                            return (result, total);
                        }
                    }
                } else {
                    fallback_needed.push(op.clone());
                }
            }
        }

        // Fetch missed UTXOs from RocksDB (only if we haven't met target yet)
        for op in fallback_needed {
            if let Some(txo) = self.fetch_from_store(&op) {
                let matches = match (&txo.asset_id, asset_id) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                } && txo.locked_until.is_none_or(|l| l <= now_ms);
                if matches {
                    if let Ok(amt) = Decimal::from_str(&txo.amount) {
                        result.push((op, txo, amt));
                        total += amt;
                        if result.len() >= limit && total >= target {
                            return (result, total);
                        }
                    }
                }
            }
        }

        (result, total)
    }

    // ─── Bootstrap rebuild ───────────────────────────────────────────

    /// Reconstruit l'index adresse, le cache supply et le cache balance
    /// à partir du contenu actuel des shards LRU. Appelé une seule fois au bootstrap.
    ///
    /// Note: au bootstrap, tous les UTXOs sont chargés dans les shards (même si certains
    /// sont évincés par le LRU). Les caches sont recalculés depuis ce qui est en mémoire,
    /// MAIS supply/balance doivent refléter l'état complet. C'est pourquoi le bootstrap
    /// dans instance.rs accumule supply/balance **avant** l'éviction LRU.
    pub async fn rebuild_indexes(&self) {
        self.address_index.clear();
        self.native_balance_cache.clear();
        self.token_balance_cache.clear();
        let mut new_supply = SupplyCache::new();

        for shard in &self.shards {
            let locked = shard.read().await;
            for (outpoint, compact) in locked.iter() {
                // Address index
                self.address_index
                    .entry(compact.address.to_string())
                    .or_insert_with(DashSet::new)
                    .insert(outpoint.clone());

                // Supply cache
                let key = compact.asset_id.as_ref().map(|a| a.to_string());
                let entry = new_supply.entry(key).or_insert((Decimal::ZERO, 0));
                entry.0 += compact.amount;
                entry.1 += 1;

                // Native balance cache
                if compact.asset_id.is_none() {
                    self.native_balance_cache
                        .entry(compact.address.to_string())
                        .and_modify(|b| *b += compact.amount)
                        .or_insert(compact.amount);
                }

                // Token balance cache
                if let Some(ref asset_arc) = compact.asset_id {
                    self.token_balance_cache
                        .entry((compact.address.clone(), asset_arc.clone()))
                        .and_modify(|b| *b += compact.amount)
                        .or_insert(compact.amount);
                }
            }
        }

        let mut cache = self.supply_cache.lock();
        *cache = new_supply;
    }

    /// Reconstruit les caches supply/balance depuis une liste complète d'UTXOs
    /// (indépendamment du contenu des shards LRU).
    /// Utilisé au bootstrap quand le nombre d'UTXOs dépasse la capacité LRU,
    /// pour que les caches reflètent l'état complet et pas seulement le cache.
    pub async fn rebuild_indexes_from_utxos(&self, all_utxos: &[(OutputId, TxOutput)]) {
        self.address_index.clear();
        self.native_balance_cache.clear();
        self.token_balance_cache.clear();
        let mut new_supply = SupplyCache::new();

        for (outpoint, txo) in all_utxos {
            let amount = Decimal::from_str(&txo.amount).unwrap_or(Decimal::ZERO);

            // Address index
            self.address_index
                .entry(txo.address.clone())
                .or_insert_with(DashSet::new)
                .insert(outpoint.clone());

            // Supply cache
            let key = txo.asset_id.clone();
            let entry = new_supply.entry(key).or_insert((Decimal::ZERO, 0));
            entry.0 += amount;
            entry.1 += 1;

            // Native balance cache
            if txo.asset_id.is_none() {
                self.native_balance_cache
                    .entry(txo.address.clone())
                    .and_modify(|b| *b += amount)
                    .or_insert(amount);
            }

            // Token balance cache
            if let Some(ref asset_id_str) = txo.asset_id {
                let addr_arc = self.interner.intern(&txo.address);
                let asset_arc = self.interner.intern(asset_id_str);
                self.token_balance_cache
                    .entry((addr_arc, asset_arc))
                    .and_modify(|b| *b += amount)
                    .or_insert(amount);
            }
        }

        let mut cache = self.supply_cache.lock();
        *cache = new_supply;
    }
}

impl Default for ShardedUtxoSet {
    fn default() -> Self {
        Self::new(0, None)
    }
}
