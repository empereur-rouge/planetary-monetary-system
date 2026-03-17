//! Implémentation RocksDB de [`LedgerDefStorage`].
//!
//! ## Schéma du Column Family
//! - `ledger_defs`: `ledger_id` → JSON sérialisé de [`LedgerDef`](pms_config::LedgerDef)
//!
//! Utilisé uniquement sur le store principal (prefix "main" ou "pms:main").
//! Les ledgers dynamiques y sont persistés lors de leur création et les
//! changements d'ownership y sont enregistrés.

use crate::{LedgerDefStorage, rocks_store::store::RocksStore};
use anyhow::Result;
use pms_config::LedgerDef;

impl LedgerDefStorage for RocksStore {
    fn get_ledger_def(&self, ledger_id: &str) -> Result<Option<LedgerDef>> {
        let cf = self.cf("ledger_defs");
        if let Some(v) = self.db.get_cf(&cf, ledger_id.as_bytes())? {
            let def: LedgerDef = serde_json::from_slice(&v)?;
            Ok(Some(def))
        } else {
            Ok(None)
        }
    }

    fn put_ledger_def(&self, def: &LedgerDef) -> Result<()> {
        let cf = self.cf("ledger_defs");
        let json = serde_json::to_vec(def)?;
        self.db.put_cf(&cf, def.id.as_bytes(), &json)?;
        Ok(())
    }

    fn list_ledger_defs(&self) -> Result<Vec<LedgerDef>> {
        let cf = self.cf("ledger_defs");
        let mut defs = Vec::new();
        for kv in self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start) {
            let (_k, v) = kv?;
            let def: LedgerDef = serde_json::from_slice(&v)?;
            defs.push(def);
        }
        Ok(defs)
    }

    fn update_owner(&self, ledger_id: &str, new_owner: Option<String>) -> Result<LedgerDef> {
        let cf = self.cf("ledger_defs");
        let raw = self.db.get_cf(&cf, ledger_id.as_bytes())?;
        let Some(raw) = raw else {
            anyhow::bail!("Ledger definition '{}' not found in RocksDB", ledger_id);
        };
        let mut def: LedgerDef = serde_json::from_slice(&raw)?;
        def.owner_pubkey = new_owner;
        let json = serde_json::to_vec(&def)?;
        self.db.put_cf(&cf, ledger_id.as_bytes(), &json)?;
        Ok(def)
    }
}
