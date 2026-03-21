// pms-server/src/api/state — AppState, FeePoolRefundSink, DAG size metric helpers.

use crate::api_keys::SharedApiKeyStore;
use crate::stats::Stats;
use pms_config::{Settings, ServerConfig, TreasuryWallets};
use pms_storage::ContractStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_wallet::Wallet;
use std::sync::{
    Arc,
    atomic::AtomicBool,
};

// ═══════════════════════════════════════════════════════════════════════════
// RefundSink implementation for pms-contracts listener
// ═══════════════════════════════════════════════════════════════════════════

/// Adapts `FeePoolRegistry` to the `RefundSink` trait required by `pms-contracts`.
///
/// Each call routes the refund to the correct per-ledger FeePool.
pub struct FeePoolRefundSink {
    pub registry: Arc<crate::fee_pool::FeePoolRegistry>,
}

impl pms_contracts::RefundSink for FeePoolRefundSink {
    fn add_burn_refund(
        &self,
        ledger_id: &str,
        address: &str,
        amount: rust_decimal::Decimal,
        asset_id: Option<String>,
    ) {
        let pool = self.registry.get_or_create(ledger_id);
        // Use try_write to avoid blocking the EventBus listener task.
        // If the lock is held (fee distribution in progress), use blocking write.
        match pool.try_write() {
            Ok(mut guard) => {
                guard.add_burn_refund(address, amount, asset_id);
            }
            Err(_) => {
                // Fallback: spawn a blocking write to avoid deadlock.
                // This is rare — only when fee distribution holds the write lock.
                let pool = pool.clone();
                let address = address.to_string();
                tokio::spawn(async move {
                    pool.write().await.add_burn_refund(&address, amount, asset_id);
                });
            }
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub srv: Arc<crate::Server>,
    pub _cfg: Arc<ServerConfig>,
    pub _ready: Arc<AtomicBool>,
    pub stats: Arc<Stats>,
    pub store: Arc<RocksStore>,
    /// Token admin déjà résolu (valeur réelle, pas "env:XXX").
    /// None = pas d'API admin active.
    pub admin_token: Option<String>,
    pub node_wallet: Arc<Wallet>,
    /// Settings for API handlers (fees, admin addresses, etc.)
    pub settings: Arc<Settings>,
    /// Parsed IP networks for admin access (from allowed_ips config)
    /// Empty = allow all with token, non-empty = whitelist mode
    pub allowed_networks: Vec<ipnetwork::IpNetwork>,
    /// Verified treasury wallet addresses (signed by coordinator)
    pub treasury_wallets: TreasuryWallets,
    /// Dynamic node registry for distributed TX processing
    pub node_registry: crate::node_registry::SharedNodeRegistry,
    /// Fee pool for accumulating fees until Milestone distribution.
    /// In multi-ledger mode, this points to the **current ledger's** pool
    /// (set by `dynamic_ledger_handler` via `fee_pool_registry`).
    pub fee_pool: crate::fee_pool::SharedFeePool,
    /// Per-ledger fee pool registry. Each ledger gets an isolated pool so
    /// that burn refunds (e.g. EDN on eden) are distributed on the correct ledger.
    pub fee_pool_registry: Arc<crate::fee_pool::FeePoolRegistry>,
    /// Multi-ledger manager (Phase 2).
    /// Quand présent, les routes /l/{ledger_id}/* sont actives.
    pub ledger_mgr: Option<Arc<pms_ledger::LedgerManager>>,
    /// Ledger ID for this request context ("main" by default).
    pub ledger_id: String,
    /// Resolved fee configuration for this ledger context.
    pub effective_fees: Arc<crate::api_fn::tx_helpers::EffectiveFees>,
    /// Store des clés API pour l'authentification des clients SDK.
    /// Protégé par un RwLock pour lectures concurrentes (middleware)
    /// et écritures exclusives (CRUD admin).
    pub api_key_store: SharedApiKeyStore,
    /// In-memory cache for activity endpoint responses.
    pub activity_cache: Arc<crate::api_fn::activity::ActivityCache>,
    /// TPS tracker for dynamic fee calculation (congestion-based multiplier).
    pub tps_tracker: Arc<pms_economics::dynamic_fee::TpsTracker>,
    /// Main EventBus for contract evaluation.
    /// Burns on ANY ledger emit `NftBurnProcessed` to this bus,
    /// where the `ContractListener` is subscribed.
    /// `None` in test contexts where contracts are not needed.
    pub contract_event_bus: Option<pms_event::EventBus>,
    /// Contract storage — always points to the **main** RocksDB store.
    /// Contracts are registered globally (via `POST /admin/contracts`) and stored
    /// in the main store's `contracts` CF. Per-ledger stores do NOT contain contracts.
    /// Used by `evaluate_transfer()` in `prepare_tx()` and `wallet_send_simple()`.
    pub contract_store: Arc<dyn ContractStorage>,
}

/// Sync the PMS_BLOCKS_TOTAL gauge with the actual in-memory DAG size for the default ledger.
pub(super) fn sync_dag_size_metric(st: &AppState) {
    sync_dag_size_metric_for(st, &st.ledger_id);
}

/// Sync PMS_BLOCKS_TOTAL for a specific ledger.
pub(super) fn sync_dag_size_metric_for(st: &AppState, ledger_id: &str) {
    if let Some(ref mgr) = st.ledger_mgr {
        if let Some(instance) = mgr.get(ledger_id) {
            crate::metrics::PMS_BLOCKS_TOTAL
                .with_label_values(&[ledger_id])
                .set(instance.dag.len() as i64);
        }
    }
}

/// Sync PMS_BLOCKS_TOTAL for all ledgers.
pub(super) fn sync_all_dag_size_metrics(st: &AppState) {
    if let Some(ref mgr) = st.ledger_mgr {
        for instance in mgr.list_all() {
            crate::metrics::PMS_BLOCKS_TOTAL
                .with_label_values(&[&instance.id])
                .set(instance.dag.len() as i64);
        }
    }
}
