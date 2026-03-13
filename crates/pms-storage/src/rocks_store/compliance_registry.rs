use crate::compliance_store::ComplianceStorage;
use crate::rocks_store::store::RocksStore;
use anyhow::Result;
use rocksdb::BoundColumnFamily;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenEntry {
    pub address: String,
    pub block_id: String,
    pub reason: String,
    pub frozen_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceLogEntry {
    pub action: String,
    pub block_id: String,
    pub target_address: Option<String>,
    pub details: serde_json::Value,
    pub timestamp_ms: i64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl RocksStore {
    fn cf_compliance_frozen(&self) -> Arc<BoundColumnFamily<'_>> {
        self.cf("compliance_frozen")
    }

    fn cf_compliance_log(&self) -> Arc<BoundColumnFamily<'_>> {
        self.cf("compliance_log")
    }

    pub fn is_frozen(&self, address: &str) -> Result<bool> {
        // Fast path: in-memory DashSet lookup (no RocksDB I/O).
        // The frozen_set is populated at bootstrap and maintained on freeze/unfreeze.
        Ok(self.frozen_set.contains(address))
    }

    pub fn get_freeze_entry(&self, address: &str) -> Result<Option<FrozenEntry>> {
        let cf = self.cf_compliance_frozen();
        if let Some(val) = self.db.get_cf(&cf, address.as_bytes())? {
            let entry: FrozenEntry = serde_json::from_slice(&val)?;
            Ok(Some(entry))
        } else {
            Ok(None)
        }
    }

    pub fn freeze_address(&self, address: &str, block_id: &str, reason: &str) -> Result<()> {
        if self.is_frozen(address)? {
            anyhow::bail!("address already frozen: {}", address);
        }
        let entry = FrozenEntry {
            address: address.to_string(),
            block_id: block_id.to_string(),
            reason: reason.to_string(),
            frozen_at_ms: now_ms(),
        };
        let cf = self.cf_compliance_frozen();
        let json = serde_json::to_vec(&entry)?;
        self.db.put_cf(&cf, address.as_bytes(), json)?;
        // Keep in-memory cache in sync
        self.frozen_set.insert(address.to_string());
        self.log_compliance_action(
            "freeze",
            block_id,
            Some(address),
            &serde_json::json!({ "reason": reason }),
        )?;
        Ok(())
    }

    pub fn unfreeze_address(&self, address: &str) -> Result<()> {
        if !self.frozen_set.contains(address) {
            anyhow::bail!("address is not frozen: {}", address);
        }
        let cf = self.cf_compliance_frozen();
        self.db.delete_cf(&cf, address.as_bytes())?;
        // Keep in-memory cache in sync
        self.frozen_set.remove(address);
        Ok(())
    }

    pub fn list_frozen(&self) -> Result<Vec<FrozenEntry>> {
        let cf = self.cf_compliance_frozen();
        let mut entries = Vec::new();
        let iter = self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start);
        for kv in iter {
            let (_, val) = kv?;
            let entry: FrozenEntry = serde_json::from_slice(&val)?;
            entries.push(entry);
        }
        Ok(entries)
    }

    pub fn log_compliance_action(
        &self,
        action: &str,
        block_id: &str,
        target_address: Option<&str>,
        details: &serde_json::Value,
    ) -> Result<()> {
        let entry = ComplianceLogEntry {
            action: action.to_string(),
            block_id: block_id.to_string(),
            target_address: target_address.map(|s| s.to_string()),
            details: details.clone(),
            timestamp_ms: now_ms(),
        };
        let cf = self.cf_compliance_log();
        let json = serde_json::to_vec(&entry)?;
        self.db.put_cf(&cf, block_id.as_bytes(), json)?;
        Ok(())
    }

    pub fn list_compliance_log(&self) -> Result<Vec<ComplianceLogEntry>> {
        let cf = self.cf_compliance_log();
        let mut entries = Vec::new();
        let iter = self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start);
        for kv in iter {
            let (_, val) = kv?;
            let entry: ComplianceLogEntry = serde_json::from_slice(&val)?;
            entries.push(entry);
        }
        Ok(entries)
    }
}

impl ComplianceStorage for RocksStore {
    fn is_frozen(&self, address: &str) -> Result<bool> {
        RocksStore::is_frozen(self, address)
    }
    fn freeze_address(&self, address: &str, block_id: &str, reason: &str) -> Result<()> {
        RocksStore::freeze_address(self, address, block_id, reason)
    }
    fn unfreeze_address(&self, address: &str) -> Result<()> {
        RocksStore::unfreeze_address(self, address)
    }
    fn list_frozen(&self) -> Result<Vec<FrozenEntry>> {
        RocksStore::list_frozen(self)
    }
    fn get_freeze_entry(&self, address: &str) -> Result<Option<FrozenEntry>> {
        RocksStore::get_freeze_entry(self, address)
    }
    fn log_compliance_action(
        &self,
        action: &str,
        block_id: &str,
        target_address: Option<&str>,
        details: &serde_json::Value,
    ) -> Result<()> {
        RocksStore::log_compliance_action(self, action, block_id, target_address, details)
    }
    fn list_compliance_log(&self) -> Result<Vec<ComplianceLogEntry>> {
        RocksStore::list_compliance_log(self)
    }
}
