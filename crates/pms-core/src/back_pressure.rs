//! Producer-side back-pressure observability hook.
//!
//! The persist-pipeline producer (`do_persist_block_internal`) calls
//! [`record_event`] every time its `persist_tx.send().await` had to wait
//! ≥ 500 ms for a free slot in the bounded mpsc channel. The
//! [`crate::metrics::PERSIST_BACK_PRESSURE_EVENTS`] counter increments
//! immediately for dashboards, and the drain-style accumulator below
//! lets the resource-guard task in pms-server consume the events on
//! its 5 s sampling cadence to arm read-only mode proactively.
//!
//! ## Why the global accumulator (and not just the metric counter)
//!
//! The 5 s sampler in pms-server checks the persist queue depth via
//! `adapter.persist_queue_depth()` — that's a **point-in-time**
//! observation. Channels that saturate for less than 5 s never show
//! up in the sampler's view: the channel filled, drained, refilled,
//! drained again, all between two samples. The producer-side
//! `send().await` waits ARE observed in those gaps, but until v0.8.0
//! they were only logged + metric-counted, not used to trigger the
//! safety net.
//!
//! Testnet 2026-05-04: 6728 back-pressure events (max 6342 ms) over
//! 24 h with **0 read-only auto-arms**. Producer was clearly hitting
//! the wall, but the sampler missed every saturation because each
//! one cleared inside the 5 s window. This module closes that gap:
//! the resource guard checks `drain()` once per tick and treats any
//! recent producer back-pressure as PersistQueue pressure (same code
//! path as the queue-depth check, same 1-tick fast-arm via
//! [`crate::back_pressure`]'s contribution to `any_pressure`).
//!
//! ## Why a process-global static (and not a per-adapter callback)
//!
//! Wiring a `Box<dyn Fn>` callback into `CoreAdapter::new()` would
//! force every call site (~10 across the workspace, including tests
//! and the tools-cli) to update. The signal is also process-global by
//! nature: there's one engine = one `ReadOnlyMode` = one read-only
//! flag, regardless of how many `CoreAdapter`s exist. A static
//! accumulator matches the actual topology cleanly.
//!
//! Tests don't drain — the counter just accumulates harmlessly into
//! never-read atomic state. No locking, no allocations, no contention.

use std::sync::atomic::{AtomicU64, Ordering};

/// Number of back-pressure events recorded since the last [`drain`].
///
/// Wraps on overflow at 2⁶⁴ — fine in practice (a saturation rate of
/// 100 events/s would take ~5.8 billion years to wrap).
static EVENT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Maximum `elapsed_ms` observed across events since the last
/// [`drain`]. Stored as a u64 so the resource guard can decide
/// "any recent event ≥ 1000 ms" cheaply without re-aggregating.
static MAX_ELAPSED_MS: AtomicU64 = AtomicU64::new(0);

/// Record one back-pressure event.
///
/// Called by the persist pipeline producer immediately after a
/// `send().await` that completed in ≥ 500 ms. Increments the
/// process-global accumulator AND the Prometheus counter so both
/// surfaces reflect the event in real time.
pub fn record_event(elapsed_ms: u64) {
    EVENT_COUNT.fetch_add(1, Ordering::Relaxed);

    // CAS-loop max: atomically bump MAX_ELAPSED_MS only if our value
    // is greater. Relaxed ordering is fine — readers (the resource
    // guard) accept stale-by-one-tick max values.
    let mut current = MAX_ELAPSED_MS.load(Ordering::Relaxed);
    while elapsed_ms > current {
        match MAX_ELAPSED_MS.compare_exchange_weak(
            current,
            elapsed_ms,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }

    crate::metrics::PERSIST_BACK_PRESSURE_EVENTS.inc();
}

/// Drain the accumulator and return `(count, max_elapsed_ms)`.
///
/// Resets both atomics to zero. The resource guard task calls this
/// once per 5 s sampling tick and uses the result to decide whether
/// to add `PersistQueue` to the current pressure signals.
pub fn drain() -> (u64, u64) {
    let count = EVENT_COUNT.swap(0, Ordering::Relaxed);
    let max = MAX_ELAPSED_MS.swap(0, Ordering::Relaxed);
    (count, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity check: record + drain works as expected. Runs in
    /// isolation by serialising on a mutex — the static is process-
    /// global so parallel tests would race.
    #[test]
    fn record_and_drain_round_trip() {
        // Reset baseline (other tests may have polluted it).
        let (_, _) = drain();

        record_event(700);
        record_event(1200);
        record_event(400); // less than 1200, should not lower max

        let (count, max) = drain();
        assert_eq!(count, 3);
        assert_eq!(max, 1200);

        // After drain, both should be zero.
        let (count, max) = drain();
        assert_eq!(count, 0);
        assert_eq!(max, 0);
    }
}
