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
    /// Serialises compliance operations (freeze / unfreeze) across concurrent
    /// admin requests. Without this, two simultaneous freeze requests for the
    /// same address could both pass the `is_frozen` check before either
    /// persisted its block, producing two audit trails for the same state
    /// change (audit finding M6). Contention is negligible — these are
    /// rare admin operations — so a single global lock is fine.
    pub compliance_lock: Arc<tokio::sync::Mutex<()>>,
    /// Coordinator sub-address shards. Empty when sharding is disabled
    /// (legacy single-address behaviour, default). When non-empty, the
    /// transaction-fee output destination round-robins across the shards
    /// via `next_coord_shard_address` to bound per-address UTXO accumulation.
    /// Audit follow-up to v0.7.4 — see `pms_wallet::shard_derivation`.
    pub coord_shard_wallets: Arc<Vec<Wallet>>,
    /// Atomic round-robin counter for `next_coord_shard_address`. Wrapped
    /// with `Arc` so `AppState::clone` shares it across handlers — that
    /// keeps the round-robin truly fair under load instead of restarting
    /// from 0 on every clone.
    pub coord_shard_round_robin: Arc<std::sync::atomic::AtomicUsize>,
}

impl AppState {
    /// Pick the next coordinator-side address that should receive a
    /// transaction-fee output. Returns the appropriate shard address
    /// when sharding is configured, otherwise falls back to the legacy
    /// `settings.admin.wallet_addresses` / `treasury_addresses` chain
    /// the handlers used pre-sharding.
    ///
    /// `hrp` is the bech32 HRP for this network (e.g. "8e" for testnet
    /// — the same value the handlers already pass to `get_address`).
    pub fn next_coord_shard_address(&self, hrp: &str) -> Option<String> {
        if self.coord_shard_wallets.is_empty() {
            // Legacy: caller falls back to settings.admin / treasury list.
            return None;
        }
        // Wrapping is intentional — `AtomicUsize::fetch_add` is atomic
        // and `n` is bounded by config validation (≤ 256), so the modulo
        // is cheap. Relaxed ordering is enough; we don't need a happens-
        // before relation across shards, just monotonic counter progress.
        let idx = self
            .coord_shard_round_robin
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % self.coord_shard_wallets.len();
        Some(self.coord_shard_wallets[idx].get_address(hrp))
    }

    /// Resolve the coordinator-side address for a transaction-fee output,
    /// applying sharding first and falling back to the legacy
    /// `admin.wallet_addresses` / `treasury_addresses` chain if sharding
    /// is disabled. Returns `None` only when none of the three sources
    /// is configured (caller should HTTP 500).
    pub fn fee_recipient_address(&self) -> Option<String> {
        let hrp = &self.settings.address.hrp;
        if let Some(s) = self.next_coord_shard_address(hrp) {
            return Some(s);
        }
        self.settings
            .admin
            .wallet_addresses
            .first()
            .cloned()
            .or_else(|| self.settings.fees.treasury_addresses.first().cloned())
    }
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
