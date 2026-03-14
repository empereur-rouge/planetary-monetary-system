use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════════════════
// Gas Pool — per-ledger anti-spam deposit
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GasPool {
    pub ledger_id: String,
    /// Current PMS balance available in pool
    pub balance: Decimal,
    /// Lifetime gas consumed by transactions
    pub total_consumed: Decimal,
    /// Lifetime PMS deposited into pool
    pub total_deposited: Decimal,
    /// Unix timestamp (ms) when pool was created
    pub created_at: i64,
}

impl GasPool {
    pub fn new(ledger_id: String, now_ms: i64) -> Self {
        Self {
            ledger_id,
            balance: Decimal::ZERO,
            total_consumed: Decimal::ZERO,
            total_deposited: Decimal::ZERO,
            created_at: now_ms,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Fee Burn — deflationary mechanism
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeeBurnResult {
    /// Amount permanently removed from circulation
    pub burned: Decimal,
    /// Amount remaining for distribution (to nodes, treasury, etc.)
    pub distributable: Decimal,
}

// ═══════════════════════════════════════════════════════════════════════════
// Dynamic Fee — congestion multiplier result
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq)]
pub struct DynamicFeeInfo {
    /// Current measured TPS
    pub current_tps: f64,
    /// Target TPS threshold
    pub target_tps: u32,
    /// Resulting fee multiplier (>= 1.0)
    pub multiplier: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gas_pool_new_has_zero_balance() {
        let pool = GasPool::new("test".into(), 1000);
        assert_eq!(pool.balance, Decimal::ZERO);
        assert_eq!(pool.total_consumed, Decimal::ZERO);
        assert_eq!(pool.total_deposited, Decimal::ZERO);
        assert_eq!(pool.ledger_id, "test");
        assert_eq!(pool.created_at, 1000);
    }

    #[test]
    fn gas_pool_serialization_roundtrip() {
        let pool = GasPool::new("eden".into(), 42);
        let json = serde_json::to_string(&pool).unwrap();
        let parsed: GasPool = serde_json::from_str(&json).unwrap();
        assert_eq!(pool, parsed);
    }
}
