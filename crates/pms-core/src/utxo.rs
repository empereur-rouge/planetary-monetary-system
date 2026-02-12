use pms_types::{OutputId, TxOutput};
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Nombre de shards (256 = 1 octet du hash)
const SHARD_COUNT: usize = 256;

/// UTXO Set partitionné en RAM pour accès concurrent.
///
/// Remplace le gros `Dag` lock pour la validation des transactions.
/// Chaque shard est protégé par un RwLock indépendant.
pub struct ShardedUtxoSet {
    shards: [RwLock<HashMap<OutputId, TxOutput>>; SHARD_COUNT],
}

impl ShardedUtxoSet {
    pub fn new() -> Self {
        // Astuce pour init un array de types non-Copy/Default simple
        let shards = std::array::from_fn(|_| RwLock::new(HashMap::new()));
        Self { shards }
    }

    /// Détermine l'index du shard (0-255) basé sur le premier octet du TxId.
    fn shard_index(out_point: &OutputId) -> usize {
        // format OutputId = { txid: String, index: u32 }
        // On prend les 2 premiers chars hex (1 octet) du txid
        let s = &out_point.txid;
        if s.len() >= 2 {
            u8::from_str_radix(&s[0..2], 16).unwrap_or(0) as usize
        } else {
            0
        }
    }

    /// Récupère un UTXO (lecture concurrente).
    pub async fn get(&self, out_point: &OutputId) -> Option<TxOutput> {
        let idx = Self::shard_index(out_point);
        let shard = self.shards[idx].read().await;
        shard.get(out_point).cloned()
    }

    /// Ajoute un UTXO (écriture ciblée).
    pub async fn add(&self, out_point: OutputId, output: TxOutput) {
        let idx = Self::shard_index(&out_point);
        let mut shard = self.shards[idx].write().await;
        shard.insert(out_point, output);
    }

    /// Supprime un UTXO (spend).
    pub async fn remove(&self, out_point: &OutputId) -> Option<TxOutput> {
        let idx = Self::shard_index(out_point);
        let mut shard = self.shards[idx].write().await;
        shard.remove(out_point)
    }

    /// Applique un delta complet (spend + create) de manière concurrente.
    /// Groups operations by shard to minimize lock acquisitions.
    pub async fn apply_diff(&self, spends: &[OutputId], creates: &[(OutputId, TxOutput)]) {
        use std::collections::HashMap as StdHashMap;

        // Group spends by shard index
        let mut spend_by_shard: StdHashMap<usize, Vec<&OutputId>> = StdHashMap::new();
        for sp in spends {
            spend_by_shard
                .entry(Self::shard_index(sp))
                .or_default()
                .push(sp);
        }

        // Group creates by shard index
        let mut create_by_shard: StdHashMap<usize, Vec<(&OutputId, &TxOutput)>> = StdHashMap::new();
        for (id, out) in creates {
            create_by_shard
                .entry(Self::shard_index(id))
                .or_default()
                .push((id, out));
        }

        // Collect all affected shard indices
        let mut affected: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        affected.extend(spend_by_shard.keys());
        affected.extend(create_by_shard.keys());

        // Process each shard with a single write lock
        for shard_idx in affected {
            let mut shard = self.shards[shard_idx].write().await;
            if let Some(sp_list) = spend_by_shard.get(&shard_idx) {
                for sp in sp_list {
                    shard.remove(*sp);
                }
            }
            if let Some(cr_list) = create_by_shard.get(&shard_idx) {
                for (id, out) in cr_list {
                    shard.insert((*id).clone(), (*out).clone());
                }
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

    /// Calcule le supply PMS natif en circulation (asset_id == None uniquement).
    ///
    /// Retourne (supply_total, nombre_utxos).
    pub async fn circulating_supply(&self) -> (rust_decimal::Decimal, usize) {
        use rust_decimal::Decimal;
        use std::str::FromStr;

        let mut total = Decimal::ZERO;
        let mut count = 0usize;

        for shard in &self.shards {
            let locked = shard.read().await;
            for (_outpoint, output) in locked.iter() {
                if output.asset_id.is_none() {
                    if let Ok(amount) = Decimal::from_str(&output.amount) {
                        total += amount;
                        count += 1;
                    }
                }
            }
        }
        (total, count)
    }

    /// Calcule le supply d'un token spécifique.
    pub async fn circulating_supply_by_asset(&self, asset_id: Option<&str>) -> (rust_decimal::Decimal, usize) {
        use rust_decimal::Decimal;
        use std::str::FromStr;

        let mut total = Decimal::ZERO;
        let mut count = 0usize;

        for shard in &self.shards {
            let locked = shard.read().await;
            for (_outpoint, output) in locked.iter() {
                let matches = match (&output.asset_id, asset_id) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                };
                if matches {
                    if let Ok(amount) = Decimal::from_str(&output.amount) {
                        total += amount;
                        count += 1;
                    }
                }
            }
        }
        (total, count)
    }

    /// Calcule la balance PMS d'une adresse (rétrocompatible).
    pub async fn balance_by_address(&self, address: &str) -> rust_decimal::Decimal {
        self.balance_by_address_and_asset(address, None).await
    }

    /// Calcule la balance d'une adresse pour un asset spécifique.
    pub async fn balance_by_address_and_asset(&self, address: &str, asset_id: Option<&str>) -> rust_decimal::Decimal {
        use rust_decimal::Decimal;
        use std::str::FromStr;

        let mut total = Decimal::ZERO;

        for shard in &self.shards {
            let locked = shard.read().await;
            for (_outpoint, output) in locked.iter() {
                if output.address == address {
                    let matches = match (&output.asset_id, asset_id) {
                        (None, None) => true,
                        (Some(a), Some(b)) => a == b,
                        _ => false,
                    };
                    if matches {
                        if let Ok(amount) = Decimal::from_str(&output.amount) {
                            total += amount;
                        }
                    }
                }
            }
        }
        total
    }

    /// Retourne tous les UTXOs d'une adresse (tous les assets).
    pub async fn utxos_by_address(&self, address: &str) -> Vec<(OutputId, TxOutput)> {
        let mut result = Vec::new();

        for shard in &self.shards {
            let locked = shard.read().await;
            for (outpoint, output) in locked.iter() {
                if output.address == address {
                    result.push((outpoint.clone(), output.clone()));
                }
            }
        }
        result
    }
}

impl Default for ShardedUtxoSet {
    fn default() -> Self {
        Self::new()
    }
}
