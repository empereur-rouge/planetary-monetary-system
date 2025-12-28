// src/atomic.rs

use crate::StoredBlock;
use crate::helpers::{key_time_index, le_to_u64, now_ms_i64, ts_to_be, u64_to_le};
use crate::mutation::LedgerMutation;
use crate::rocks_store::store::RocksStore;
use anyhow::{Result, bail};
use pms_errors::ValidationError;
use pms_types::{Block, TxOutput};
use pms_wire::WireMeta;
use rocksdb::WriteBatch;

impl RocksStore {
    #[inline]
    fn k_time_prefix(&self) -> Vec<u8> {
        // ex: b"pms:<ns>:time:"
        format!("{}:time:", self.prefix).into_bytes()
    }

    #[inline]
    fn k_time_row(&self, ts_ms: i64, id: &str) -> Vec<u8> {
        // clé triable par ordre lexicographique: prefix + ts big-endian + ":" + id
        // 8 octets BE -> ordre chrono
        let mut k = self.k_time_prefix();
        let be = (ts_ms as i128).to_be_bytes(); // 16 octets si tu veux “futur-proof” (sinon i64 ok)
        k.extend_from_slice(&be);
        k.push(b':');
        k.extend_from_slice(id.as_bytes());
        k
    }
    pub async fn append_block_atomic(&self, b: &StoredBlock) -> Result<bool> {
        use rocksdb::WriteBatch;

        // 0. existe déjà ?
        let cf_blocks = self.cf("blocks");
        if self.db.get_cf(cf_blocks, b.id.as_bytes())?.is_some() {
            return Ok(false);
        }

        // timestamp pour index_by_time & tip
        let now_ts = now_ms_i64();
        let time_key = key_time_index(now_ts, &b.id);

        let cf_idx = self.cf("idx_blocks");
        let cf_time = self.cf("by_time");
        let cf_i2t = self.cf("id2ts");
        let cf_tips = self.cf("tips");
        let cf_count = self.cf("children_count");
        let cf_childset = self.cf("children_set");

        // On construit le batch
        let mut batch = WriteBatch::default();

        // blocks[id] = StoredBlock JSON
        let json = serde_json::to_vec(b)?;
        batch.put_cf(cf_blocks, b.id.as_bytes(), &json);

        // idx_blocks[id] = ""
        batch.put_cf(cf_idx, b.id.as_bytes(), b"");

        // by_time[ time_key ] = ""
        batch.put_cf(cf_time, &time_key, b"");

        // id2ts[id] = ts_be(8)
        batch.put_cf(cf_i2t, b.id.as_bytes(), ts_to_be(now_ts));

        // tips: new block becomes tip
        batch.put_cf(cf_tips, b.id.as_bytes(), ts_to_be(now_ts));

        // tips: remove its parents from tips (they’re no longer tips)
        for p in &b.parents {
            batch.delete_cf(cf_tips, p.as_bytes());
        }

        // children_count[parent]++ ; children_set[parent|0x00|child] = ""
        for p in &b.parents {
            // read old count
            let old = self.db.get_cf(cf_count, p.as_bytes())?;
            let newcount = match old {
                Some(v) if v.len() == 8 => le_to_u64(&v) + 1,
                _ => 1u64,
            };
            batch.put_cf(cf_count, p.as_bytes(), u64_to_le(newcount));

            let mut edge_key = Vec::with_capacity(p.len() + 1 + b.id.len());
            edge_key.extend_from_slice(p.as_bytes());
            edge_key.push(0);
            edge_key.extend_from_slice(b.id.as_bytes());
            batch.put_cf(cf_childset, edge_key, b"");
        }

        // write atomiquement
        self.db.write(batch)?;

        // ⚠ trim by_time / tip_limit :
        //    on réutilise la logique de index_by_time() pour ne pas garder trop d'entrées.
        //    (tu peux soit factoriser dans une fn privée, soit refaire le code ici)
        self.trim_by_time()?;
        self.trim_tips()?;

        Ok(true)
    }

    pub async fn persist_genesis(&self, g: &Block, meta: &WireMeta) -> Result<()> {
        // Construit un StoredBlock équivalent
        let sb = StoredBlock {
            id: g.id.clone(),
            parents: vec![],
            payload_json: serde_json::to_string(&g.payload).ok(),
            nonce: g.nonce,

            // 🔥 nouveau : champs "réseau + signature"
            network_id: meta.network_id.clone(),
            protocol_version: meta.protocol_version as u16,
            signer_pk_hex: "GENESIS".to_string(), // marqueur spécial
            signature_hex: String::new(),         // pas de signature pour l’instant
        };

        // Passe par l’append atomique (Rocks) pour rester cohérent
        self.append_block_atomic(&sb)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    pub(crate) fn apply_dag_indices(&self, batch: &mut WriteBatch, b: &StoredBlock) -> Result<()> {
        let now_ts = now_ms_i64();
        let time_key = key_time_index(now_ts, &b.id);

        let cf_blocks = self.cf("blocks");
        let cf_idx = self.cf("idx_blocks");
        let cf_time = self.cf("by_time");
        let cf_i2t = self.cf("id2ts");
        let cf_tips = self.cf("tips");
        let cf_count = self.cf("children_count");
        let cf_childset = self.cf("children_set");

        let json = serde_json::to_vec(b)?;
        batch.put_cf(cf_blocks, b.id.as_bytes(), &json);
        batch.put_cf(cf_idx, b.id.as_bytes(), b"");
        batch.put_cf(cf_time, &time_key, b"");
        batch.put_cf(cf_i2t, b.id.as_bytes(), ts_to_be(now_ts));
        batch.put_cf(cf_tips, b.id.as_bytes(), ts_to_be(now_ts));

        for p in &b.parents {
            batch.delete_cf(cf_tips, p.as_bytes());
        }

        for p in &b.parents {
            let old = self.db.get_cf(cf_count, p.as_bytes())?;
            let newcount = match old {
                Some(v) if v.len() == 8 => le_to_u64(&v) + 1,
                _ => 1u64,
            };
            batch.put_cf(cf_count, p.as_bytes(), u64_to_le(newcount));

            let mut edge_key = Vec::with_capacity(p.len() + 1 + b.id.len());
            edge_key.extend_from_slice(p.as_bytes());
            edge_key.push(0);
            edge_key.extend_from_slice(b.id.as_bytes());
            batch.put_cf(cf_childset, edge_key, b"");
        }

        Ok(())
    }

    pub(crate) fn apply_ledger_mutation(
        &self,
        batch: &mut WriteBatch,
        ledger: &LedgerMutation<'_>,
        b: &StoredBlock,
    ) -> Result<()> {
        match ledger {
            LedgerMutation::None => Ok(()),

            LedgerMutation::Mint { block_id, outputs } => {
                // cf utxo
                let cf_utxo = self.cf("utxo");

                // pour chaque output: utxo[(block_id, idx)] = montant/adresse
                for (idx, out) in outputs.iter().enumerate() {
                    let key = RocksStore::utxo_key(block_id, idx as u32);
                    // ici tu peux vérifier qu'elle n'existe pas déjà si tu veux
                    let val = serde_json::to_vec(out)?;
                    batch.put_cf(cf_utxo, &key, &val);
                }
                Ok(())
            }

            LedgerMutation::TxUtxo { tx } => {
                let cf_utxo = self.cf("utxo");
                let cf_tx_applied = self.cf("tx_applied");

                // 1) check inputs encore présents (lecture hors batch)
                for inp in &tx.inputs {
                    let key = RocksStore::utxo_key(&inp.out.txid, inp.out.index);
                    let exists = self.db.get_cf(cf_utxo, &key)?;
                    if exists.is_none() {
                        bail!("UTXO input absent dans store (double-spend ou état désync)");
                    }
                }

                // 2) delete inputs
                for inp in &tx.inputs {
                    let key = RocksStore::utxo_key(&inp.out.txid, inp.out.index);
                    batch.delete_cf(cf_utxo, &key);
                }

                // 3) create outputs
                for (idx, out) in tx.outputs.iter().enumerate() {
                    let key = RocksStore::utxo_key(&b.id, idx as u32);
                    let val = serde_json::to_vec(out)?;
                    batch.put_cf(cf_utxo, &key, &val);
                }

                // 4) marquer la tx appliquée
                batch.put_cf(cf_tx_applied, b.id.as_bytes(), b"");

                Ok(())
            }
        }
    }

    /// Construit une clé UTXO stable: "<txid>#<index>"
    pub(crate) fn utxo_key(txid: &str, index: u32) -> Vec<u8> {
        format!("{txid}#{index}").into_bytes()
    }
}
