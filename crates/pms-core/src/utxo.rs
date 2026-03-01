use dashmap::{DashMap, DashSet};
use pms_types::{OutputId, TxOutput};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

/// Nombre de shards (256 = 1 octet du hash)
const SHARD_COUNT: usize = 256;

/// Cache du supply par asset_id : (total, count).
type SupplyCache = HashMap<Option<String>, (Decimal, usize)>;

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
}

impl CompactOutput {
    fn to_tx_output(&self) -> TxOutput {
        TxOutput {
            address: self.address.to_string(),
            amount: self.amount.to_string(),
            asset_id: self.asset_id.as_ref().map(|a| a.to_string()),
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
        let mut set = match self.set.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
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

/// UTXO Set partitionné en RAM pour accès concurrent.
///
/// Remplace le gros `Dag` lock pour la validation des transactions.
/// Chaque shard est protégé par un RwLock indépendant.
///
/// Inclut un index secondaire par adresse (DashMap) et un cache du supply
/// pour éviter les scans O(n) sur les requêtes balance / supply.
///
/// Stocke en interne des `CompactOutput` (~32 bytes) au lieu de `TxOutput`
/// (~148 bytes) grâce à l'interning des adresses et Decimal stack-allocated.
pub struct ShardedUtxoSet {
    shards: [RwLock<HashMap<OutputId, CompactOutput>>; SHARD_COUNT],
    /// Index secondaire : address -> set of OutputIds
    address_index: DashMap<String, DashSet<OutputId>>,
    /// Cache supply incrémental : asset_id -> (total_amount, utxo_count)
    supply_cache: Mutex<SupplyCache>,
    /// Per-address native (PMS) balance cache — avoids shard read lock starvation
    /// under continuous write load (e.g. heavy token minting).
    native_balance_cache: DashMap<String, Decimal>,
    /// Interner pour dédupliquer addresses et asset_ids
    interner: Interner,
}

impl ShardedUtxoSet {
    pub fn new() -> Self {
        let shards = std::array::from_fn(|_| RwLock::new(HashMap::new()));
        Self {
            shards,
            address_index: DashMap::new(),
            supply_cache: Mutex::new(SupplyCache::new()),
            native_balance_cache: DashMap::new(),
            interner: Interner::new(),
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
        }
    }

    // ─── Supply cache helpers (private) ──────────────────────────────

    fn supply_add_compact(&self, output: &CompactOutput) {
        let mut cache = match self.supply_cache.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
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
    }

    fn supply_sub_compact(&self, output: &CompactOutput) {
        let mut cache = match self.supply_cache.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let key = output.asset_id.as_ref().map(|a| a.to_string());
        if let Some(entry) = cache.get_mut(&key) {
            entry.0 -= output.amount;
            entry.1 = entry.1.saturating_sub(1);
            if entry.1 == 0 {
                cache.remove(&key);
            }
        }
        drop(cache);

        // Maintain native balance cache
        if output.asset_id.is_none() {
            if let Some(mut entry) = self.native_balance_cache.get_mut(&*output.address) {
                *entry.value_mut() -= output.amount;
            }
        }
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

    /// Récupère un UTXO (lecture concurrente).
    pub async fn get(&self, out_point: &OutputId) -> Option<TxOutput> {
        let idx = Self::shard_index(out_point);
        let shard = self.shards[idx].read().await;
        shard.get(out_point).map(|c| c.to_tx_output())
    }

    /// Ajoute un UTXO (écriture ciblée).
    pub async fn add(&self, out_point: OutputId, output: TxOutput) {
        let compact = self.compact(&output);
        let address = compact.address.to_string();
        self.supply_add_compact(&compact);
        let idx = Self::shard_index(&out_point);
        {
            let mut shard = self.shards[idx].write().await;
            shard.insert(out_point.clone(), compact);
        } // shard write lock dropped before touching DashMap
        self.addr_index_add(&address, &out_point);
    }

    /// Supprime un UTXO (spend).
    pub async fn remove(&self, out_point: &OutputId) -> Option<TxOutput> {
        let idx = Self::shard_index(out_point);
        let removed = {
            let mut shard = self.shards[idx].write().await;
            shard.remove(out_point)
        }; // shard write lock dropped here before touching DashMap
        if let Some(ref compact) = removed {
            self.addr_index_remove(&compact.address, out_point);
            self.supply_sub_compact(compact);
        }
        removed.map(|c| c.to_tx_output())
    }

    /// Applique un delta complet (spend + create) de manière concurrente.
    /// Groups operations by shard to minimize lock acquisitions.
    /// DashMap index updates are deferred until AFTER shard locks are released
    /// to prevent deadlock with balance/utxo queries (which take DashMap → shard).
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

        // Process each shard with a single write lock
        for shard_idx in affected {
            {
                let mut shard = self.shards[shard_idx].write().await;

                // Spends: lookup address before removal, defer index update
                if let Some(sp_list) = spend_by_shard.get(&shard_idx) {
                    for sp in sp_list {
                        if let Some(compact) = shard.remove(*sp) {
                            deferred_index_ops.push((
                                compact.address.to_string(),
                                (*sp).clone(),
                                false, // remove
                            ));
                            self.supply_sub_compact(&compact);
                        }
                    }
                }

                // Creates: insert into shard, defer index update
                if let Some(cr_list) = create_by_shard.get(&shard_idx) {
                    for (id, compact) in cr_list.iter().map(|item| (&item.0, &item.1)) {
                        deferred_index_ops.push((
                            compact.address.to_string(),
                            id.clone(),
                            true, // add
                        ));
                        self.supply_add_compact(compact);
                        shard.insert(id.clone(), CompactOutput {
                            address: compact.address.clone(),
                            amount: compact.amount,
                            asset_id: compact.asset_id.clone(),
                        });
                    }
                }
            } // shard write lock dropped here
        }

        // Apply deferred DashMap index updates (no shard lock held)
        for (address, outpoint, is_add) in deferred_index_ops {
            if is_add {
                self.addr_index_add(&address, &outpoint);
            } else {
                self.addr_index_remove(&address, &outpoint);
            }
        }
    }

    /// Taille totale approximative (somme des lens) - lent car lock tout
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
        let cache = match self.supply_cache.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        cache.get(&None).cloned().unwrap_or((Decimal::ZERO, 0))
    }

    /// Retourne le supply d'un token spécifique depuis le cache.
    pub async fn circulating_supply_by_asset(
        &self,
        asset_id: Option<&str>,
    ) -> (Decimal, usize) {
        let cache = match self.supply_cache.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
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
    /// Native (None): lecture directe du cache O(1), pas de shard lock.
    /// Token (Some): utilise l'index secondaire + shard read locks.
    pub async fn balance_by_address_and_asset(
        &self,
        address: &str,
        asset_id: Option<&str>,
    ) -> Decimal {
        // Native PMS: use the lock-free balance cache
        if asset_id.is_none() {
            return self.balance_by_address(address).await;
        }

        // Token balance: shard-based approach (not in hot path during heavy minting)
        // Collect OutputIds first, then DROP the DashMap guard before awaiting shard locks.
        let out_points: Vec<OutputId> = match self.address_index.get(address) {
            Some(utxo_ids) => utxo_ids.iter().map(|r| r.key().clone()).collect(),
            None => return Decimal::ZERO,
        };

        // Group by shard index to acquire each lock only once
        let mut by_shard: HashMap<usize, Vec<OutputId>> = HashMap::new();
        for op in out_points {
            by_shard.entry(Self::shard_index(&op)).or_default().push(op);
        }

        let mut total = Decimal::ZERO;
        for (shard_idx, ops) in by_shard {
            let shard = self.shards[shard_idx].read().await;
            for op in &ops {
                if let Some(compact) = shard.get(op) {
                    let matches = match (&compact.asset_id, asset_id) {
                        (Some(a), Some(b)) => a.as_ref() == b,
                        _ => false,
                    };
                    if matches {
                        total += compact.amount;
                    }
                }
            }
        }

        total
    }

    /// Retourne tous les UTXOs d'une adresse (tous les assets).
    /// Utilise l'index secondaire.
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
        for (shard_idx, ops) in by_shard {
            let shard = self.shards[shard_idx].read().await;
            for op in ops {
                if let Some(compact) = shard.get(&op) {
                    result.push((op, compact.to_tx_output()));
                }
            }
        }

        result
    }

    // ─── Bootstrap rebuild ───────────────────────────────────────────

    /// Reconstruit l'index adresse, le cache supply et le cache balance
    /// à partir du contenu actuel. Appelé une seule fois au bootstrap.
    pub async fn rebuild_indexes(&self) {
        self.address_index.clear();
        self.native_balance_cache.clear();
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
            }
        }

        let mut cache = match self.supply_cache.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        *cache = new_supply;
    }
}

impl Default for ShardedUtxoSet {
    fn default() -> Self {
        Self::new()
    }
}
