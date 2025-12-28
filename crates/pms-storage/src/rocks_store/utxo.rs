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
    /// Sorties : (address, amount_decstr)
    pub outputs: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoDelta {
    pub spend: Vec<(String, u32)>,                  // (txid, index)
    pub create: Vec<(String, u32, String, String)>, // (txid, index, address, amount)
}

impl RocksStore {
    #[inline]
    fn k_utxo_key(out_txid: &str, out_index: u32) -> Vec<u8> {
        // même schéma que Redis: "TXID:index"
        // clé binaire = b"<txid>:<index>"
        let mut v = Vec::with_capacity(out_txid.len() + 1 + 10);
        v.extend_from_slice(out_txid.as_bytes());
        v.push(b':');
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
            .get_cf(cf_applied, Self::k_tx_applied_key(&tx.txid))?
            .is_some()
        {
            return Ok(false);
        }

        // 2) Phase check: toutes les entrées existent ?
        let cf_utxo = self.cf_utxo();
        for (ptx, idx) in &tx.inputs {
            let key = Self::k_utxo_key(ptx, *idx);
            if self.db.get_cf(cf_utxo, &key)?.is_none() {
                // au moins une entrée manquante -> conflit / double dépense
                return Ok(false);
            }
        }

        // 3) Construction du batch (delete inputs, put outputs, mark applied)
        let mut batch = WriteBatch::default();

        // Consomme les entrées
        for (ptx, idx) in &tx.inputs {
            let key = Self::k_utxo_key(ptx, *idx);
            batch.delete_cf(cf_utxo, key);
        }

        // Crée les sorties: outpoints = `${txid}:${i}`
        #[derive(Serialize)]
        struct OutVal<'a> {
            addr: &'a str,
            amt: &'a str,
        }

        for (i, (addr, amount)) in tx.outputs.iter().enumerate() {
            let key = Self::k_utxo_key(&tx.txid, i as u32);
            let val = OutVal { addr, amt: amount };
            let json = serde_json::to_vec(&val)?;
            batch.put_cf(cf_utxo, key, json);
        }

        // Marque la transaction comme appliquée (idempotence future)
        batch.put_cf(cf_applied, Self::k_tx_applied_key(&tx.txid), b"");

        // 4) Commit atomique
        self.db.write(batch)?;
        Ok(true)
    }
}

/// Petit utilitaire pour encoder un u32 en ASCII sans allocs inutiles.
/// (Tu peux simplement faire `format!("{index}")` si tu préfères la simplicité.)
#[inline]
fn index_to_ascii(idx: u32) -> String {
    // simple & lisible, la micro-optimisation n'est pas critique
    idx.to_string()
}
