// pms-server/src/api/tasks — Background tasks (fee distribution, inflation mint, activity backfill).

use super::state::AppState;
use crate::read_only::ReadOnlyReason;
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

                // Skip the entire round when the engine is in read-only
                // mode — distribution produces a Reward block, and we
                // promised callers no new blocks while armed. Fees keep
                // accumulating in the pool; once the guard disarms the
                // next tick will flush them out without loss.
                if state_distrib.read_only.is_armed() {
                    tracing::debug!(
                        target = "fee_distribution",
                        reason = state_distrib.read_only.reason().as_str(),
                        "skipping fee distribution: engine is read-only"
                    );
                    continue;
                }

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

                // Same rationale as `spawn_fee_distributor_task`: skip
                // the round under read-only mode. Inflation mint is a
                // strictly additive operation — deferring a single
                // round just delays inflation by `interval_sec`,
                // which is harmless.
                if state_inflation.read_only.is_armed() {
                    tracing::debug!(
                        target = "inflation_mint",
                        reason = state_inflation.read_only.reason().as_str(),
                        "skipping inflation mint: engine is read-only"
                    );
                    continue;
                }

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

        // Per-ledger memory of the previous bloom counter values so
        // we can convert the per-store cumulative atomics into the
        // global Prometheus IntCounters with correct deltas.
        let mut prev_bloom: std::collections::HashMap<String, (u64, u64)> =
            std::collections::HashMap::new();

        // Per-ledger memory of the previous `pms_blocks_persisted_total`
        // counter value + the previous EWMA reading. Used to compute the
        // smoothed `pms_blocks_per_second_ewma` gauge each tick — see
        // `update_blocks_per_second_ewma` below for the math.
        let mut prev_blocks: std::collections::HashMap<String, (u64, f64)> =
            std::collections::HashMap::new();

        loop {
            interval.tick().await;

            // ---- main ledger (always present) ----------------------
            sample_ledger(
                &state.ledger_id,
                &*state.srv.adapter_arc(),
                &state.fee_pool,
                state.store.is_write_stopped(),
                &state.store,
                &mut prev_bloom,
            )
            .await;
            update_blocks_per_second_ewma(&state.ledger_id, &mut prev_blocks);

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
                        &instance.store,
                        &mut prev_bloom,
                    )
                    .await;
                    update_blocks_per_second_ewma(&lid, &mut prev_blocks);
                }
            }
        }
    });
}

/// Update the smoothed `pms_blocks_per_second_ewma{ledger_id}` gauge
/// from the cumulative `pms_blocks_persisted_total{ledger_id}` counter.
///
/// **EWMA** (exponentially weighted moving average) with `α = 0.2`:
///
/// ```text
///   instant_rate = (current_count - previous_count) / SAMPLE_INTERVAL_SECS
///   ewma         = α * instant_rate + (1 - α) * previous_ewma
/// ```
///
/// `α = 0.2` means each new 5 s sample contributes 20 % to the
/// displayed value while 80 % is retained from history — effective
/// smoothing window ~25 s. Tuned to match the natural cadence of the
/// `background_persist_task` drain bursts (one WriteBatch every
/// ~5-10 s under load) so a single batch landing inside one sample
/// window doesn't move the displayed rate by more than ~20 % of its
/// peak. The dashboard reading this gauge sees the **honest sustained
/// throughput**, never the sub-second drain-burst artifacts.
///
/// First sample (no prior counter value yet) is skipped — we'd need to
/// know the boot time to compute a meaningful rate, and reporting 0 or
/// `current_count / sample_interval` would both be misleading.
fn update_blocks_per_second_ewma(
    ledger_id: &str,
    prev: &mut std::collections::HashMap<String, (u64, f64)>,
) {
    const SAMPLE_INTERVAL_SECS: f64 = 5.0;
    const ALPHA: f64 = 0.2;

    let cur_count = crate::metrics::BLOCKS_PERSISTED
        .with_label_values(&[ledger_id])
        .get();

    match prev.get(ledger_id).copied() {
        None => {
            // First observation — store the baseline, don't publish a
            // rate yet (nothing to compare against). The next tick
            // produces the first real EWMA value.
            prev.insert(ledger_id.to_string(), (cur_count, 0.0));
        }
        Some((prev_count, prev_ewma)) => {
            // Counter is monotonic — `cur_count >= prev_count` always.
            // Use saturating subtraction to be safe against any future
            // counter reset (e.g. a Prometheus client library bug we
            // can't see today).
            let delta = cur_count.saturating_sub(prev_count);
            let instant_rate = delta as f64 / SAMPLE_INTERVAL_SECS;
            let ewma = ALPHA * instant_rate + (1.0 - ALPHA) * prev_ewma;

            crate::metrics::BLOCKS_PER_SECOND_EWMA
                .with_label_values(&[ledger_id])
                .set(ewma);

            prev.insert(ledger_id.to_string(), (cur_count, ewma));
        }
    }
}

/// Sample one ledger's gauges and feed them into the Prometheus registry.
/// Defined out-of-line so the loop above stays readable.
async fn sample_ledger(
    ledger_id: &str,
    adapter: &(dyn pms_interface::NetDagAdapter + 'static),
    fee_pool: &crate::fee_pool::SharedFeePool,
    is_write_stopped: Option<bool>,
    store: &pms_storage::rocks_store::store::RocksStore,
    prev_bloom: &mut std::collections::HashMap<String, (u64, u64)>,
) {
    use crate::metrics::{
        FEE_POOL_TOTAL, PERSIST_QUEUE_CAPACITY, PERSIST_QUEUE_DEPTH,
        ROCKSDB_WRITE_STALLED_SECONDS, UTXO_SET_SIZE,
    };
    use std::sync::atomic::Ordering;

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

    // Bloom filter outcome counters. The per-store atomics are
    // monotonic; we publish the delta against our last sample to feed
    // the global Prometheus IntCounter (which is also monotonic).
    let cur_skips = store.bloom_skips.load(Ordering::Relaxed);
    let cur_hits = store.bloom_hits.load(Ordering::Relaxed);
    let (prev_s, prev_h) = prev_bloom
        .get(ledger_id)
        .copied()
        .unwrap_or((0, 0));
    if cur_skips > prev_s {
        pms_core::metrics::PERSIST_BLOOM_SKIPS.inc_by(cur_skips - prev_s);
    }
    if cur_hits > prev_h {
        pms_core::metrics::PERSIST_BLOOM_HITS.inc_by(cur_hits - prev_h);
    }
    prev_bloom.insert(ledger_id.to_string(), (cur_skips, cur_hits));
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

/// Spawns the resource-guard task (v0.7.23) — graceful read-only degradation.
///
/// On a fixed cadence (5 s) this task samples three pressure signals:
///
///   1. **cgroup memory** (used / max). On Linux + Docker (the production
///      target) this maps to the cgroup limit set by `mem_limit` in
///      `docker-compose.*.yml`, so the watcher arms read-only mode
///      *before* the kernel OOM killer fires — preferring a 503 to
///      clients over a SIGKILL that would lose the persist channel.
///   2. **Free disk percent** on the RocksDB volume. Distinct from
///      the `[health].min_disk_free_percent` "degraded" signal: this
///      threshold (`disk_critical_free_percent`) is lower so a slow-
///      leaking disk first surfaces as a healthz warning, then gates
///      writes when actually critical.
///   3. **RocksDB stall**: `is-write-stopped` (canonical signal) and
///      L0 file count crossing `rocksdb_l0_critical_files`. The L0
///      check is an early-warning that fires before RocksDB's own
///      hard `level0_stop_writes_trigger` so writes degrade gracefully
///      to 503 instead of blocking indefinitely on the producer side.
///
/// **Hysteresis** prevents flapping: ARM after 2 consecutive samples
/// (10 s) above the high watermark; DISARM after 6 consecutive samples
/// (30 s) below the low watermark. A `Manual` arm via
/// `POST /admin/read-only/arm` is **never** auto-cleared — only the
/// matching `disarm` endpoint releases it (so an operator can hold
/// the engine in read-only state during maintenance without the
/// guard fighting them).
///
/// When `[health].read_only_guard_enabled` is `false`, this task is a
/// no-op (returns immediately on spawn). Recommended `false` only for
/// benchmarks and local tests where the guard would interfere; every
/// production deployment should leave it on.
pub fn spawn_resource_guard_task(state: AppState) {
    if !state.settings.health.read_only_guard_enabled {
        tracing::info!(
            target = "read_only_guard",
            "Resource guard task disabled (health.read_only_guard_enabled = false)"
        );
        return;
    }

    let high_pct = state.settings.health.memory_high_watermark_pct;
    let low_pct = state.settings.health.memory_low_watermark_pct;
    let disk_critical_pct = state.settings.health.disk_critical_free_percent;
    let l0_critical = state.settings.health.rocksdb_l0_critical_files;
    let min_arm_duration = Duration::from_secs(state.settings.health.read_only_min_arm_duration_secs);
    let rocks_path = std::path::PathBuf::from(&state.settings.rocks.path);

    if low_pct >= high_pct {
        tracing::error!(
            target = "read_only_guard",
            high_pct, low_pct,
            "memory_low_watermark_pct ({:.1}) >= memory_high_watermark_pct ({:.1}); \
             disabling resource guard to prevent flapping. Fix the config and restart.",
            low_pct, high_pct
        );
        return;
    }

    tokio::spawn(async move {
        const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
        // ARM after 2 consecutive over-watermark samples (10 s).
        // DISARM after 6 consecutive under-watermark samples (30 s) —
        // intentionally asymmetric: armed is cheap (rejects writes,
        // already-running ops keep going), so we err on the side of
        // staying armed a little longer than strictly necessary.
        const ARM_TICKS: u32 = 2;
        const DISARM_TICKS: u32 = 6;

        tracing::info!(
            target = "read_only_guard",
            high_pct,
            low_pct,
            disk_critical_pct,
            l0_critical,
            arm_ticks = ARM_TICKS,
            disarm_ticks = DISARM_TICKS,
            min_arm_duration_secs = min_arm_duration.as_secs(),
            interval_secs = SAMPLE_INTERVAL.as_secs(),
            "Resource guard task started"
        );

        let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
        // Consume immediate first tick so the first real sample lands
        // SAMPLE_INTERVAL after boot — gives RocksDB time to settle
        // and avoids a spurious arm during a heavy boot-time backfill.
        interval.tick().await;

        let mut over_count: u32 = 0;
        let mut under_count: u32 = 0;
        // When the watcher arms (auto), record the wall-clock instant
        // so we can enforce `min_arm_duration` before allowing an
        // auto-disarm. Without this floor, a memtable burst that
        // crosses the high watermark for ~10 s then drops 30 s later
        // when RocksDB flushes loops the engine in/out of read-only
        // every ~1-2 minutes (testnet incident 2026-05-01). Manual
        // arms set this to `None` so the operator's `disarm` releases
        // immediately.
        let mut armed_at: Option<std::time::Instant> = None;

        loop {
            interval.tick().await;

            // ─── 1. Memory check ────────────────────────────────────
            // While clear, arm at >= high_pct. While armed, only clear
            // at < low_pct — that's the hysteresis band.
            let mem_pressure = match read_cgroup_memory_pct() {
                Some(pct) => {
                    if state.read_only.is_armed() {
                        pct >= low_pct
                    } else {
                        pct >= high_pct
                    }
                }
                None => false, // no cgroup info → don't arm on memory
            };

            // ─── 2. Disk check ──────────────────────────────────────
            let disk_pressure = crate::api_fn::healthz::disk_free_percent(&rocks_path)
                .map(|free_pct| free_pct < disk_critical_pct)
                .unwrap_or(false);

            // ─── 3. RocksDB stall check ─────────────────────────────
            let rocks_stalled = matches!(state.store.is_write_stopped(), Some(true));
            let rocks_l0_critical = state
                .store
                .l0_files()
                .map(|n| n >= l0_critical)
                .unwrap_or(false);
            let rocks_pressure = rocks_stalled || rocks_l0_critical;

            // ─── 4. Decide ──────────────────────────────────────────
            let any_pressure = mem_pressure || disk_pressure || rocks_pressure;
            let new_reason = if mem_pressure {
                ReadOnlyReason::Memory
            } else if disk_pressure {
                ReadOnlyReason::Disk
            } else if rocks_pressure {
                ReadOnlyReason::RocksDb
            } else {
                ReadOnlyReason::None
            };

            let was_armed = state.read_only.is_armed();
            let cur_reason = state.read_only.reason();

            if any_pressure {
                under_count = 0;
                over_count = over_count.saturating_add(1);

                if !was_armed && over_count >= ARM_TICKS {
                    state.read_only.arm(new_reason);
                    crate::metrics::ENGINE_READ_ONLY.set(1);
                    armed_at = Some(std::time::Instant::now());
                    tracing::warn!(
                        target = "read_only_guard",
                        reason = new_reason.as_str(),
                        consecutive_samples = over_count,
                        "🛑 Engine entering READ-ONLY mode — writes will return 503"
                    );
                } else if was_armed
                    && cur_reason != ReadOnlyReason::Manual
                    && cur_reason != new_reason
                {
                    // Already armed by the watcher, but the dominant
                    // reason changed (e.g. memory cleared but disk
                    // tripped). Update so the metric / healthz / 503
                    // body reflect the live cause. Don't override a
                    // Manual arm — operator decisions take precedence.
                    state.read_only.arm(new_reason);
                    tracing::warn!(
                        target = "read_only_guard",
                        prev_reason = cur_reason.as_str(),
                        new_reason = new_reason.as_str(),
                        "Read-only reason changed"
                    );
                }
            } else {
                over_count = 0;
                if was_armed {
                    if cur_reason == ReadOnlyReason::Manual {
                        // Operator-armed: never auto-disarm.
                        under_count = 0;
                    } else {
                        under_count = under_count.saturating_add(1);
                        // Anti-flap floor (v0.7.26): even when pressure
                        // has cleared for DISARM_TICKS samples, refuse
                        // to disarm before `min_arm_duration` has
                        // elapsed since the arm. Breaks the memtable-
                        // flush cycle that previously toggled the
                        // engine in/out of read-only every 1-2 min.
                        let min_duration_elapsed = armed_at
                            .map(|t| t.elapsed() >= min_arm_duration)
                            .unwrap_or(true);
                        if under_count >= DISARM_TICKS && min_duration_elapsed {
                            let prev = state.read_only.disarm();
                            crate::metrics::ENGINE_READ_ONLY.set(0);
                            under_count = 0;
                            let armed_for_secs = armed_at
                                .map(|t| t.elapsed().as_secs())
                                .unwrap_or(0);
                            armed_at = None;
                            tracing::info!(
                                target = "read_only_guard",
                                prev_reason = prev.as_str(),
                                consecutive_samples = DISARM_TICKS,
                                armed_for_secs,
                                "✅ Engine exiting READ-ONLY mode — writes accepted again"
                            );
                        } else if under_count >= DISARM_TICKS {
                            // Pressure has cleared but the min-arm
                            // floor hasn't elapsed yet — log at debug
                            // so an operator can see the floor at
                            // work without spamming warn.
                            let armed_for_secs = armed_at
                                .map(|t| t.elapsed().as_secs())
                                .unwrap_or(0);
                            tracing::debug!(
                                target = "read_only_guard",
                                armed_for_secs,
                                min_arm_duration_secs = min_arm_duration.as_secs(),
                                "Pressure cleared but holding read-only until min-arm floor elapses"
                            );
                        }
                    }
                }
            }
        }
    });
}

/// Read cgroup memory usage as a percentage of its limit, **excluding
/// reclaimable memory** (page cache + reclaimable slabs).
///
/// Returns `None` when the cgroup interface is unavailable (non-Linux,
/// fallback paths missing) or the cgroup has no memory limit set —
/// in those cases the resource guard simply skips the memory check.
///
/// **Why subtract reclaimable** (v0.7.26): the OOM killer triggers
/// on irreclaimable memory (anonymous heap, kernel slabs that can't
/// be returned). Page cache and reclaimable slabs are released by
/// the kernel automatically under pressure. Counting them toward
/// our 90 % watermark would arm read-only on a process that is in
/// fact perfectly healthy. On the testnet engine the file cache is
/// only ~100 MiB so this is mostly future-proofing, but it's the
/// correct semantics — and matches what `docker stats` displays in
/// its memory column (which subtracts cache for the same reason).
///
/// Tries cgroup v2 first (`/sys/fs/cgroup/memory.{current,max,stat}`)
/// which is the default on Debian 12 + Docker 29.x (the production
/// target), then falls back to cgroup v1 for older hosts. v1 fallback
/// does NOT subtract reclaimable — those hosts are old enough that
/// the file format differs and the precision isn't worth the complexity.
fn read_cgroup_memory_pct() -> Option<f64> {
    // cgroup v2
    if let (Ok(cur_str), Ok(max_str)) = (
        std::fs::read_to_string("/sys/fs/cgroup/memory.current"),
        std::fs::read_to_string("/sys/fs/cgroup/memory.max"),
    ) {
        let cur: u64 = cur_str.trim().parse().ok()?;
        let max_trim = max_str.trim();
        if max_trim == "max" {
            return None;
        }
        let max: u64 = max_trim.parse().ok()?;
        if max == 0 {
            return None;
        }
        // Subtract reclaimable categories. None means we couldn't
        // read memory.stat — fall back to raw `current` rather than
        // skip the whole check (better signal than no signal).
        let reclaimable = read_cgroup_v2_reclaimable().unwrap_or(0);
        let effective = cur.saturating_sub(reclaimable);
        return Some((effective as f64 / max as f64) * 100.0);
    }

    // cgroup v1 fallback
    let cur: u64 = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.usage_in_bytes")
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let max: u64 = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
        .ok()?
        .trim()
        .parse()
        .ok()?;
    // cgroup v1 reports a huge sentinel (~9.2 EB) when no memory limit
    // is set. Treat anything north of 1 EB as "no limit".
    if max == 0 || max > (1u64 << 60) {
        return None;
    }
    Some((cur as f64 / max as f64) * 100.0)
}

/// Sum the reclaimable memory categories from
/// `/sys/fs/cgroup/memory.stat` (cgroup v2) — page cache + reclaimable
/// slabs. Returns `None` when the file is unreadable or unparseable.
///
/// Only `file` (page cache) and `slab_reclaimable` are counted as
/// reclaimable. Anonymous (`anon`), kernel stacks, page tables, and
/// `slab_unreclaimable` all stay in the irreclaimable footprint
/// because they can't be evicted under memory pressure.
fn read_cgroup_v2_reclaimable() -> Option<u64> {
    let s = std::fs::read_to_string("/sys/fs/cgroup/memory.stat").ok()?;
    let mut file: u64 = 0;
    let mut slab_reclaimable: u64 = 0;
    for line in s.lines() {
        let mut parts = line.split_whitespace();
        let key = match parts.next() {
            Some(k) => k,
            None => continue,
        };
        let val: u64 = match parts.next().and_then(|v| v.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        match key {
            "file" => file = val,
            "slab_reclaimable" => slab_reclaimable = val,
            _ => {}
        }
    }
    Some(file + slab_reclaimable)
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
