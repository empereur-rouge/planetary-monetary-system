use crate::rocks_store::store::RocksStore;
use anyhow::Result;
use rocksdb::WriteBatch;
use serde::{Deserialize, Serialize};

/// Représentation minimale d’une TX pour l’appliquer au set UTXO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoApply {
    pub txid: String,
    /// Entrées : (prev_txid, index)
    pub inputs: Vec<(String, u32)>,
    /// Sorties : (address, amount_decstr, asset_id)
    pub outputs: Vec<(String, String, Option<String>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoDelta {
    pub spend: Vec<(String, u32)>, // (txid, index)
    pub create: Vec<(String, u32, String, String, Option<String>)>, // (txid, index, address, amount, asset_id)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoValue {
    #[serde(rename = "addr")]
    pub address: String,
    #[serde(rename = "amt")]
    pub amount: String,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "ast")]
    pub asset_id: Option<String>,
}

impl RocksStore {
    #[inline]
    fn k_utxo_key(out_txid: &str, out_index: u32) -> Vec<u8> {
        // Standardized on '#' separator (matches make_utxo_key in store.rs)
        let mut v = Vec::with_capacity(out_txid.len() + 1 + 10);
        v.extend_from_slice(out_txid.as_bytes());
        v.push(b'#');
        v.extend_from_slice(index_to_ascii(out_index).as_bytes());
        v
    }

    #[inline]
    fn k_tx_applied_key(txid: &str) -> &[u8] {
        // clé = txid (valeur vide)
        txid.as_bytes()
    }

    /// Applique une transaction au set UTXO de manière atomique (via batch).
    ///
    /// - `Ok(true)`  : appliquée (inputs existants, non appliquée auparavant)
    /// - `Ok(false)` : conflit (entrée manquante) **ou** déjà appliquée
    ///
    /// ⚠️ Remarque concurrence:
    /// - `WriteBatch` rend l’écriture **atomique**,
    ///   mais la phase de **vérification** est faite en lecture simple.
    /// - Si tu as des **écritures concurrentes multi-process**, envisage
    ///   `TransactionDB` (RocksDB) avec snapshot + CAS pour une stricte sérialisation.
    pub async fn utxo_apply_tx_atomic(&self, tx: &UtxoApply) -> Result<bool> {
        // 1) Idempotence: déjà appliquée ?
        let cf_applied = self.cf_tx_applied();
        if self
            .db
            .get_cf(&cf_applied, Self::k_tx_applied_key(&tx.txid))?
            .is_some()
        {
            return Ok(false);
        }

        // 2) Phase check: toutes les entrées existent ?
        let cf_utxo = self.cf_utxo();
        for (ptx, idx) in &tx.inputs {
            let key = Self::k_utxo_key(ptx, *idx);
            if self.db.get_cf(&cf_utxo, &key)?.is_none() {
                // au moins une entrée manquante -> conflit / double dépense
                return Ok(false);
            }
        }

        // 3) Construction du batch (delete inputs, put outputs, mark applied)
        let mut batch = WriteBatch::default();

        // Consomme les entrées
        for (ptx, idx) in &tx.inputs {
            let key = Self::k_utxo_key(ptx, *idx);
            batch.delete_cf(&cf_utxo, key);
        }

        // Crée les sorties: outpoints = `${txid}:${i}`
        #[derive(Serialize)]
        struct OutVal<'a> {
            addr: &'a str,
            amt: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            ast: Option<&'a str>,
        }

        for (i, (addr, amount, asset_id)) in tx.outputs.iter().enumerate() {
            let key = Self::k_utxo_key(&tx.txid, i as u32);
            let val = OutVal {
                addr,
                amt: amount,
                ast: asset_id.as_deref(),
            };
            let json = serde_json::to_vec(&val)?;
            batch.put_cf(&cf_utxo, key, json);
        }

        // Marque la transaction comme appliquée (idempotence future)
        batch.put_cf(&cf_applied, Self::k_tx_applied_key(&tx.txid), b"");

        // 4) Commit atomique
        self.db.write(batch)?;
        Ok(true)
    }
    pub fn get_utxo(&self, txid: &str, index: u32) -> Result<Option<UtxoValue>> {
        let cf_utxo = self.cf_utxo();
        let key = Self::k_utxo_key(txid, index);
        if let Some(val) = self.db.get_cf(&cf_utxo, key)? {
            let u: UtxoValue = serde_json::from_slice(&val)?;
            Ok(Some(u))
        } else {
            Ok(None)
        }
    }

    /// Iterate all unspent UTXOs from the `utxo` CF.
    /// Returns (txid, index, UtxoValue) for each entry.
    /// This is the authoritative UTXO state (never pruned, unlike the in-memory DAG).
    ///
    /// **Warning**: loads ALL UTXOs into a Vec. For large UTXO sets, prefer
    /// [`stream_all_utxos`] which uses O(buffer_size) memory.
    pub fn iter_all_utxos(&self) -> Result<Vec<(String, u32, UtxoValue)>> {
        let cf = self.cf_utxo();
        let mut out = Vec::new();
        for item in self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start) {
            let (key, val) = item?;
            let key_str =
                std::str::from_utf8(&key).map_err(|e| anyhow::anyhow!("invalid utxo key: {e}"))?;
            let (txid, idx) = parse_utxo_key(key_str)?;
            let uv: UtxoValue = serde_json::from_slice(&val)?;
            out.push((txid, idx, uv));
        }
        Ok(out)
    }

    /// Stream all unspent UTXOs from the `utxo` CF through a bounded channel.
    ///
    /// Unlike [`iter_all_utxos`], this uses O(buffer_size) memory instead of
    /// O(total_utxos). Intended to be called from a dedicated OS thread
    /// (not the tokio runtime) via `std::thread::spawn`.
    ///
    /// Returns the number of UTXOs sent. If the receiver is dropped,
    /// iteration stops early and the count of items sent so far is returned.
    pub fn stream_all_utxos(
        &self,
        tx: std::sync::mpsc::SyncSender<(String, u32, UtxoValue)>,
    ) -> Result<usize> {
        let cf = self.cf_utxo();
        let mut count: usize = 0;
        for item in self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start) {
            let (key, val) = item?;
            let key_str =
                std::str::from_utf8(&key).map_err(|e| anyhow::anyhow!("invalid utxo key: {e}"))?;
            let (txid, idx) = parse_utxo_key(key_str)?;
            let uv: UtxoValue = serde_json::from_slice(&val)?;
            if tx.send((txid, idx, uv)).is_err() {
                break; // receiver dropped
            }
            count += 1;
        }
        Ok(count)
    }
}

/// Petit utilitaire pour encoder un u32 en ASCII sans allocs inutiles.
/// (Tu peux simplement faire `format!("{index}")` si tu préfères la simplicité.)
#[inline]
fn index_to_ascii(idx: u32) -> String {
    // simple & lisible, la micro-optimisation n'est pas critique
    idx.to_string()
}

/// Parse a UTXO key in format "{txid}#{index}" (or legacy "{txid}:{index}") into (txid, index).
fn parse_utxo_key(key: &str) -> Result<(String, u32)> {
    // Try '#' first (standard format), then ':' (legacy) for backward compat
    let sep_pos = key
        .rfind('#')
        .or_else(|| key.rfind(':'))
        .ok_or_else(|| anyhow::anyhow!("invalid utxo key format (no ':' or '#'): {key}"))?;
    let txid = key[..sep_pos].to_string();
    let idx: u32 = key[sep_pos + 1..]
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid utxo key index: {e}"))?;
    Ok((txid, idx))
}
