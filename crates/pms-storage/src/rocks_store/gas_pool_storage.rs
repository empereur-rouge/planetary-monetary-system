//! Implémentation RocksDB de GasPoolStorage.
//!
//! ## Schéma du Column Family:
//! - `gas_pools`: `ledger_id` -> JSON sérialisé de `GasPool`
//!
//! Opérations atomiques read-modify-write sur chaque pool.

use crate::{GasPoolStorage, rocks_store::store::RocksStore};
use anyhow::{Result, bail};
use pms_types_economics::GasPool;
use rust_decimal::Decimal;

impl GasPoolStorage for RocksStore {
    fn get_gas_pool(&self, ledger_id: &str) -> Result<Option<GasPool>> {
        let cf = self.cf("gas_pools");
        if let Some(v) = self.db.get_cf(&cf, ledger_id.as_bytes())? {
            let pool: GasPool = serde_json::from_slice(&v)?;
            Ok(Some(pool))
        } else {
            Ok(None)
        }
    }

    fn put_gas_pool(&self, pool: &GasPool) -> Result<()> {
        let cf = self.cf("gas_pools");
        let json = serde_json::to_vec(pool)?;
        self.db.put_cf(&cf, pool.ledger_id.as_bytes(), &json)?;
        Ok(())
    }

    fn list_gas_pools(&self) -> Result<Vec<GasPool>> {
        let cf = self.cf("gas_pools");
        let mut pools = Vec::new();
        for kv in self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start) {
            let (_k, v) = kv?;
            let pool: GasPool = serde_json::from_slice(&v)?;
            pools.push(pool);
        }
        Ok(pools)
    }

    fn consume_gas(
        &self,
        ledger_id: &str,
        gas_per_tx: Decimal,
        min_balance: Decimal,
    ) -> Result<Decimal> {
        let cf = self.cf("gas_pools");
        let raw = self.db.get_cf(&cf, ledger_id.as_bytes())?;
        let Some(raw) = raw else {
            bail!("No gas pool for ledger '{ledger_id}'");
        };
        let mut pool: GasPool = serde_json::from_slice(&raw)?;

        let new_balance = pms_economics::gas_pool::consume(&mut pool, gas_per_tx, min_balance)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let json = serde_json::to_vec(&pool)?;
        self.db.put_cf(&cf, ledger_id.as_bytes(), &json)?;
        Ok(new_balance)
    }

    fn deposit_gas(&self, ledger_id: &str, amount: Decimal) -> Result<Decimal> {
        let cf = self.cf("gas_pools");
        let mut pool = match self.db.get_cf(&cf, ledger_id.as_bytes())? {
            Some(raw) => serde_json::from_slice(&raw)?,
            None => {
                let now_ms = chrono::Utc::now().timestamp_millis();
                GasPool::new(ledger_id.to_string(), now_ms)
            }
        };

        let new_balance = pms_economics::gas_pool::deposit(&mut pool, amount)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let json = serde_json::to_vec(&pool)?;
        self.db.put_cf(&cf, ledger_id.as_bytes(), &json)?;
        Ok(new_balance)
    }

    fn withdraw_gas(&self, ledger_id: &str, amount: Decimal) -> Result<Decimal> {
        let cf = self.cf("gas_pools");
        let raw = self.db.get_cf(&cf, ledger_id.as_bytes())?;
        let Some(raw) = raw else {
            bail!("No gas pool for ledger '{ledger_id}'");
        };
        let mut pool: GasPool = serde_json::from_slice(&raw)?;

        let new_balance = pms_economics::gas_pool::withdraw(&mut pool, amount)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let json = serde_json::to_vec(&pool)?;
        self.db.put_cf(&cf, ledger_id.as_bytes(), &json)?;
        Ok(new_balance)
    }
}
