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

/// Spawns the activity-retention task (audit follow-up to v0.7.4).
///
/// When `[health].activity_retention_days` is set in the config, this
/// task wakes every 24 hours, computes
/// `cutoff = now - retention_days × 86400 × 1000`, and calls
/// `RocksStore::purge_activity_before(cutoff)` so the
/// `addr_activity` / `addr_type_activity` / `activity_items` CFs stay
/// bounded over time.
///
/// `None` retention disables the task entirely (default — preserves
/// pre-0.7.4 behaviour for existing deployments). The compliance log
/// is intentionally NOT touched by this task: it's regulatory audit
/// material and the operator must explicitly opt in via
/// `POST /admin/purge-compliance-log`.
pub fn spawn_activity_retention_task(state: AppState) {
    let Some(retention_days) = state.settings.health.activity_retention_days else {
        return;
    };
    if retention_days == 0 {
        tracing::warn!(
            target = "activity_retention",
            "activity_retention_days = 0 ignored (would purge everything every day); \
             set None to disable, or a positive integer for a real retention window"
        );
        return;
    }

    tokio::spawn(async move {
        // First sweep happens 1h after boot — gives the engine time to
        // settle before we add scan pressure on the activity CFs.
        let initial_delay = Duration::from_secs(3600);
        let interval_dur = Duration::from_secs(86_400); // 24h between sweeps

        tracing::info!(
            target = "activity_retention",
            retention_days,
            initial_delay_secs = initial_delay.as_secs(),
            interval_secs = interval_dur.as_secs(),
            "activity retention task started"
        );

        tokio::time::sleep(initial_delay).await;
        let mut interval = tokio::time::interval(interval_dur);
        // Consume the immediate first tick — we already slept above.
        interval.tick().await;

        loop {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let cutoff_ms = now_ms.saturating_sub(
                (retention_days.saturating_mul(86_400_000)) as i64,
            );

            let store = state.store.clone();
            let cutoff = cutoff_ms;
            let result = tokio::task::spawn_blocking(move || {
                store.purge_activity_before(cutoff)
            })
            .await;

            match result {
                Ok(Ok(stats)) => {
                    tracing::info!(
                        target = "activity_retention",
                        scanned = stats.scanned,
                        deleted = stats.deleted,
                        cutoff_ms = stats.cutoff_ms,
                        "activity retention sweep done"
                    );
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        target = "activity_retention",
                        error = %e,
                        "activity retention sweep FAILED — will retry on next interval"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        target = "activity_retention",
                        error = %e,
                        "activity retention task panicked — will retry on next interval"
                    );
                }
            }

            interval.tick().await;
        }
    });
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

/// Spawns the auto UTXO-consolidation task (recommendation #5, v0.7.5).
///
/// Bounds the per-address UTXO accumulation that fee receipts produce
/// at the coordinator's master address. Without this task an operator
/// has to remember to `POST /admin/consolidate-utxos` periodically;
/// with it the engine self-heals on a schedule.
///
/// Enabled by setting `[health].auto_consolidate_interval_secs` to a
/// positive value (recommended: `600` = 10 min). The task fires every
/// `interval_secs`, queries the UTXO count at the coordinator master
/// address, and triggers `admin_consolidate_utxos` only when the count
/// exceeds `auto_consolidate_min_utxos` (default `200`). Below the
/// threshold the loop iteration is a no-op.
///
/// The task does NOT consolidate sub-address shards — `coord_shard_count`
/// already bounds per-shard accumulation by routing fees round-robin
/// across N addresses. If a deployment runs without sharding (count=0)
/// AND with sustained heavy fee traffic, this task is the safety net.
///
/// Coordinator-only (skipped on non-coordinator nodes).
pub fn spawn_consolidation_task(state: AppState) {
    let Some(interval_sec) = state.settings.health.auto_consolidate_interval_secs else {
        return;
    };
    if interval_sec == 0 {
        return;
    }
    let min_utxos = state.settings.health.auto_consolidate_min_utxos;

    tokio::spawn(async move {
        // Initial delay so we don't fight the boot-time activity backfill
        // for the storage write lock.
        tokio::time::sleep(Duration::from_secs(60)).await;
        let mut interval = tokio::time::interval(Duration::from_secs(interval_sec));
        // Consume immediate first tick — we already slept.
        interval.tick().await;

        let hrp = state.settings.address.hrp.clone();
        let addr = state.node_wallet.get_address(&hrp);
        tracing::info!(
            target = "consolidation",
            address = %addr,
            interval_secs = interval_sec,
            min_utxos,
            "auto-consolidation task started"
        );

        loop {
            interval.tick().await;

            // Cheap UTXO count check via the adapter. utxos_by_address
            // clones the OutputId list out of the address index — at
            // 200+ UTXOs that's still microseconds, not milliseconds.
            let adapter = state.srv.adapter_arc();
            let utxos = adapter.utxos_by_address(&addr).await;
            let count = utxos.len();

            if count < min_utxos {
                tracing::debug!(
                    target = "consolidation",
                    address = %addr,
                    count,
                    min_utxos,
                    "auto-consolidation: below threshold, skipping"
                );
                continue;
            }

            tracing::info!(
                target = "consolidation",
                address = %addr,
                count,
                "auto-consolidation: triggering self-transfer"
            );

            // Reuse the public admin handler — it already does the full
            // forge + sign + persist + fee-pool accumulation cycle.
            // Calling it directly (rather than over HTTP) avoids needing
            // the admin token + a self-loopback reqwest client. The
            // returned `(StatusCode, Json)` tuple is discarded; logs
            // inside the handler tell the operator what happened.
            let req = crate::api_fn::consolidation::ConsolidateRequest {
                asset_id: None,
                max_inputs: 64,
            };
            let _ = crate::api_fn::consolidation::admin_consolidate_utxos(
                axum::extract::State(state.clone()),
                axum::Json(req),
            )
            .await;
        }
    });
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
