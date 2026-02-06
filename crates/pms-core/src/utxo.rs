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
    /// Ceci est appelé APRÈS la persistance réussie dans RocksDB.
    pub async fn apply_diff(&self, spends: &[OutputId], creates: &[(OutputId, TxOutput)]) {
        // Note: Idéalement on grouperait par shard pour lock une seule fois par shard.
        // Ici on fait simple pour commencer.

        // Spends
        for sp in spends {
            self.remove(sp).await;
        }

        // Creates
        for (id, out) in creates {
            self.add(id.clone(), out.clone()).await;
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

    /// Calcule le supply total en circulation (somme de tous les UTXOs).
    ///
    /// Cette méthode itère sur tous les shards et additionne les montants.
    /// Retourne (supply_total, nombre_utxos).
    ///
    /// **Note**: Opération potentiellement lente car elle lock tous les shards.
    pub async fn circulating_supply(&self) -> (rust_decimal::Decimal, usize) {
        use rust_decimal::Decimal;
        use std::str::FromStr;

        let mut total = Decimal::ZERO;
        let mut count = 0usize;

        for shard in &self.shards {
            let locked = shard.read().await;
            for (_outpoint, output) in locked.iter() {
                if let Ok(amount) = Decimal::from_str(&output.amount) {
                    total += amount;
                    count += 1;
                }
            }
        }
        (total, count)
    }

    /// Calcule la balance d'une adresse en parcourant tous les UTXOs.
    ///
    /// **Note**: Opération potentiellement lente car elle lock tous les shards.
    pub async fn balance_by_address(&self, address: &str) -> rust_decimal::Decimal {
        use rust_decimal::Decimal;
        use std::str::FromStr;

        let mut total = Decimal::ZERO;

        for shard in &self.shards {
            let locked = shard.read().await;
            for (_outpoint, output) in locked.iter() {
                if output.address == address {
                    if let Ok(amount) = Decimal::from_str(&output.amount) {
                        total += amount;
                    }
                }
            }
        }
        total
    }

    /// Retourne tous les UTXOs d'une adresse.
    ///
    /// **Note**: Opération potentiellement lente car elle lock tous les shards.
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
