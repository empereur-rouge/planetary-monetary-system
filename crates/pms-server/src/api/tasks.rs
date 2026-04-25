// pms-server/src/api/tasks — Background tasks (fee distribution, inflation mint, activity backfill).

use super::state::AppState;
use pms_storage::DagStorage;
use std::sync::Arc;
use std::time::Duration;

/// Spawns the fee distribution task if enabled in configuration.
/// Public for testing integration.
pub fn spawn_fee_distributor_task(state: AppState) {
    let settings = &state.settings;
    if settings.fees.distribution_interval_sec > 0 {
        let state_distrib = state.clone();
        let interval_sec = settings.fees.distribution_interval_sec;

        // Only run if Coordinator or Dev
        tokio::spawn(async move {
            tracing::info!(
                "⏰ Fee Distribution Service started (interval: {}s)",
                interval_sec
            );
            let mut interval = tokio::time::interval(Duration::from_secs(interval_sec));

            // consume first tick (immediate)
            interval.tick().await;

            loop {
                interval.tick().await; // Wait for next tick

                // 1. Distribute main ledger
                distribute_for_ledger(&state_distrib).await;

                // 2. Distribute each custom ledger from the registry
                if let Some(ref mgr) = state_distrib.ledger_mgr {
                    for lid in mgr.list_ids() {
                        if lid == "main" {
                            continue;
                        }

                        let pool = state_distrib.fee_pool_registry.get_or_create(&lid);
                        // Skip if pool is empty (no fees/refunds pending)
                        if !pool.read().await.has_fees() {
                            continue;
                        }

                        // Build per-ledger AppState with correct adapter/store/pool
                        if let Some(instance) = mgr.get(&lid) {
                            let mut ledger_state = state_distrib.clone();
                            ledger_state.srv = crate::Server::api_only(
                                instance.adapter.clone(),
                                &instance.def.network_id,
                                instance.def.protocol_version,
                                state_distrib.node_wallet.clone(),
                                Some(state_distrib.srv.broadcast_sender()),
                            );
                            ledger_state.store = instance.store.clone();
                            ledger_state.ledger_id = lid.clone();
                            ledger_state.fee_pool = pool;
                            ledger_state.effective_fees = Arc::new(
                                crate::api_fn::tx_helpers::resolve_effective_fees(
                                    &state_distrib.settings.fees,
                                    instance.def.fees.as_ref(),
                                ),
                            );

                            distribute_for_ledger(&ledger_state).await;
                        }
                    }
                }
            }
        });
    }
}

/// Run fee distribution for a single ledger's AppState.
pub async fn distribute_for_ledger(state: &AppState) {
    let pool_total = state.fee_pool.read().await.total_fees;

    match crate::fee_distribution::perform_fee_distribution(state, None).await {
        Ok(res) => {
            if res.success && res.total_distributed != "0" {
                tracing::info!(
                    "✅ [{}] Fee distribution: {} PMS to {} recipients",
                    state.ledger_id,
                    res.total_distributed,
                    res.num_recipients
                );
            } else if !res.success {
                tracing::warn!(
                    pool_total = %pool_total,
                    ledger = %state.ledger_id,
                    "⚠️ [{}] Fee distribution FAILED (success=false). \
                     Possible causes: empty tips (DAG over-pruned) or not coordinator. \
                     Fees are accumulating and NOT being distributed.",
                    state.ledger_id
                );
            }
            // Note: success=true with total=0 means no fees to distribute (normal)
        }
        Err(e) => {
            tracing::error!(
                pool_total = %pool_total,
                ledger = %state.ledger_id,
                error = %e,
                "❌ [{}] Fee distribution ERROR — fees blocked! \
                 Pool has {} PMS waiting. Error: {}",
                state.ledger_id, pool_total, e
            );
        }
    }
}

/// Spawns the scheduled inflation mint task if enabled in configuration.
pub fn spawn_inflation_mint_task(state: AppState) {
    let settings = &state.settings;
    if settings.fees.daily_inflation_enabled && settings.fees.annual_inflation_percent > 0.0 {
        let state_inflation = state.clone();
        let interval_sec = settings.fees.daily_inflation_interval_sec;

        tokio::spawn(async move {
            tracing::info!(
                "📊 Inflation Mint Service started (interval: {}s, rate: {}%/year)",
                interval_sec,
                state_inflation.settings.fees.annual_inflation_percent
            );
            let mut interval = tokio::time::interval(Duration::from_secs(interval_sec));

            // consume first tick (immediate)
            interval.tick().await;

            loop {
                interval.tick().await;

                match crate::fee_distribution::perform_daily_inflation_mint(&state_inflation).await
                {
                    Ok(res) => {
                        if res.success && res.total_distributed != "0" {
                            tracing::info!(
                                "📊 Inflation mint success: {} PMS distributed",
                                res.total_distributed
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!("❌ Inflation mint failed: {}", e);
                    }
                }
            }
        });
    }
}

/// Spawns the metrics sampler (item 4, v0.7.4).
///
/// Most operator-facing signals are gauges that can't be incremented from
/// the hot path — the persist queue depth, the in-memory UTXO set size,
/// the fee pool balance — so we read them on a fixed cadence (5s) and
/// publish into Prometheus. The same loop drives the global RocksDB
/// stall counter by polling `is-write-stopped` and adding the sampling
/// interval whenever it's `1`.
///
/// 5s is a deliberate compromise: tight enough that a stall surfaces in
/// the next Prometheus scrape, loose enough that the sampler itself is
/// not visible on a flame graph. Each tick walks `ledger_mgr.list_ids()`
/// — that's bounded (1 main + N custom ledgers) and each call is O(1)
/// against in-memory state.
pub fn spawn_metrics_sampler_task(state: AppState) {
    const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

    tokio::spawn(async move {
        tracing::info!(
            target = "pms_metrics",
            "Metrics sampler started (interval: {:?})",
            SAMPLE_INTERVAL
        );
        let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
        // Consume the first immediate tick so samples start one full
        // interval after boot — gives the rest of the system time to
        // settle and avoids reporting transient zero values.
        interval.tick().await;

        loop {
            interval.tick().await;

            // ---- main ledger (always present) ----------------------
            sample_ledger(
                &state.ledger_id,
                &*state.srv.adapter_arc(),
                &state.fee_pool,
                state.store.is_write_stopped(),
            )
            .await;

            // ---- custom ledgers from the registry ------------------
            if let Some(ref mgr) = state.ledger_mgr {
                for lid in mgr.list_ids() {
                    if lid == "main" {
                        continue;
                    }
                    let Some(instance) = mgr.get(&lid) else {
                        continue;
                    };
                    let pool = state.fee_pool_registry.get_or_create(&lid);
                    sample_ledger(
                        &lid,
                        &*instance.adapter,
                        &pool,
                        instance.store.is_write_stopped(),
                    )
                    .await;
                }
            }
        }
    });
}

/// Sample one ledger's gauges and feed them into the Prometheus registry.
/// Defined out-of-line so the loop above stays readable.
async fn sample_ledger(
    ledger_id: &str,
    adapter: &(dyn pms_interface::NetDagAdapter + 'static),
    fee_pool: &crate::fee_pool::SharedFeePool,
    is_write_stopped: Option<bool>,
) {
    use crate::metrics::{
        FEE_POOL_TOTAL, PERSIST_QUEUE_CAPACITY, PERSIST_QUEUE_DEPTH,
        ROCKSDB_WRITE_STALLED_SECONDS, UTXO_SET_SIZE,
    };

    // Persist queue gauges. `None` from the adapter means the backend
    // doesn't expose this introspection (e.g. mock adapters in tests);
    // skip rather than publish a misleading zero.
    if let Some((depth, capacity)) = adapter.persist_queue_depth() {
        PERSIST_QUEUE_DEPTH
            .with_label_values(&[ledger_id])
            .set(depth as i64);
        PERSIST_QUEUE_CAPACITY
            .with_label_values(&[ledger_id])
            .set(capacity as i64);
    }

    if let Some(size) = adapter.utxo_set_size().await {
        UTXO_SET_SIZE
            .with_label_values(&[ledger_id])
            .set(size as i64);
    }

    // Fee pool: read the snapshot under a brief read lock. We don't hold
    // it across `await` boundaries — the lock is dropped at end of expr.
    let total = fee_pool.read().await.total_fees;
    FEE_POOL_TOTAL
        .with_label_values(&[ledger_id])
        .set(total.to_string().parse::<f64>().unwrap_or(0.0));

    // RocksDB stall counter: increment by the sampling interval *only*
    // when the property explicitly reads `true`. `None` means we don't
    // know — better to under-report than to over-report a stall.
    if matches!(is_write_stopped, Some(true)) {
        ROCKSDB_WRITE_STALLED_SECONDS.inc_by(5);
    }
}

/// Spawns a background task to backfill missing `activity_items` entries.
///
/// Runs once at startup (after a 30s stabilization delay) to ensure 100%
/// fast-path coverage for activity/history queries. Without pre-computed
/// `activity_items`, ~30% of blocks fall back to block fetch + classification
/// (10-50ms instead of 1-2ms per page).
///
/// Enabled via `rocks.auto_reindex_activity_items = true` (default: true).
/// Idempotent — safe to run on every restart.
pub fn spawn_activity_backfill_task(state: AppState) {
    if !state.settings.rocks.auto_reindex_activity_items {
        return;
    }

    let store = state.store.clone();
    let ledger_id = state.ledger_id.clone();

    tokio::spawn(async move {
        // Wait for system stabilization before background work
        tokio::time::sleep(Duration::from_secs(30)).await;

        tracing::info!(
            ledger = %ledger_id,
            "Starting activity_items backfill (background)..."
        );

        // Run the reindex in a blocking task to avoid stalling the async runtime
        let store_bg = store.clone();
        let lid = ledger_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            store_bg.reindex_all_activity_items()
        })
        .await;

        match result {
            Ok(Ok(stats)) => {
                if stats.indexed > 0 {
                    tracing::info!(
                        ledger = %ledger_id,
                        indexed = stats.indexed,
                        skipped_encrypted = stats.skipped_encrypted,
                        total = stats.total_blocks,
                        "Activity items backfill complete"
                    );
                } else {
                    tracing::info!(
                        ledger = %ledger_id,
                        "Activity items: all blocks already indexed"
                    );
                }
            }
            Ok(Err(e)) => {
                tracing::error!(
                    ledger = %lid,
                    error = %e,
                    "Activity items backfill failed"
                );
            }
            Err(e) => {
                tracing::error!(
                    ledger = %lid,
                    error = %e,
                    "Activity items backfill task panicked"
                );
            }
        }
    });
}
