use crate::rocks_store::compliance_registry::{ComplianceLogEntry, FrozenEntry};
use anyhow::Result;

pub trait ComplianceStorage: Send + Sync {
    fn is_frozen(&self, address: &str) -> Result<bool>;
    fn freeze_address(&self, address: &str, block_id: &str, reason: &str) -> Result<()>;
    fn unfreeze_address(&self, address: &str) -> Result<()>;
    fn list_frozen(&self) -> Result<Vec<FrozenEntry>>;
    fn get_freeze_entry(&self, address: &str) -> Result<Option<FrozenEntry>>;
    fn log_compliance_action(
        &self,
        action: &str,
        block_id: &str,
        target_address: Option<&str>,
        details: &serde_json::Value,
    ) -> Result<()>;
    fn list_compliance_log(&self) -> Result<Vec<ComplianceLogEntry>>;
}
