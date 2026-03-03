use serde::{Deserialize, Serialize};

/// Pre-computed activity item stored in the `activity_items` column family.
///
/// Fields like `block_id`, `ts_ms`, and `ledger_id` are NOT stored here because
/// they are derived from the key / request context at read time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredActivityItem {
    pub activity_type: String,
    pub direction: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterparty: Option<String>,
    pub payload: serde_json::Value,
}
