//! Read-only mode (graceful degradation under resource pressure).
//!
//! When the engine detects that it is approaching a hard failure
//! boundary — cgroup memory near the Docker limit, free disk near
//! exhaustion, or RocksDB reporting `is-write-stopped` — it flips a
//! lock-free atomic flag that:
//!
//!   1. Causes write-producing API routes (tx, mint, burn, faucet,
//!      bridge transfer, compliance freeze/seize/reverse, etc.) to
//!      return `503 Service Unavailable` with `{"error": "read_only",
//!      "reason": "..."}` and a `Retry-After` header.
//!   2. Pauses background block-producing tasks (fee distribution,
//!      inflation mint).
//!   3. Updates the Prometheus gauge `pms_engine_read_only` so an
//!      operator can alert on sustained read-only states.
//!
//! Reads (balance, supply, history, blocks, dashboard streams) keep
//! serving normally, so users see their state without disruption while
//! the engine waits for compaction / disk pressure to clear.
//!
//! The watcher lives in [`crate::api::tasks::spawn_resource_guard_task`]
//! and applies hysteresis (arm after 2 consecutive samples over the
//! high watermark, disarm after 6 consecutive samples below the low
//! watermark) so the system doesn't flap on transient spikes.
//!
//! The intent is **safety over availability for writes** — a 503 is
//! always preferable to a Docker SIGKILL because the latter loses
//! whatever blocks were sitting in the persist channel buffer when
//! the cgroup OOM killer fired.

use std::sync::atomic::{AtomicU8, Ordering};

/// Why the engine is in read-only mode.
///
/// Discriminant values are part of the public surface — they appear in
/// `/healthz` JSON, `/admin/read-only/status` JSON, and the 503 body
/// when a write is rejected. Reordering them is a breaking change for
/// SDK consumers that branch on `reason`.
#[repr(u8)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ReadOnlyReason {
    /// Not in read-only mode.
    None = 0,
    /// cgroup memory usage crossed the high watermark.
    Memory = 1,
    /// Free disk space dropped below the critical threshold.
    Disk = 2,
    /// RocksDB reported `is-write-stopped == 1` or its L0 file count
    /// crossed the critical threshold (proxy for "compaction can't
    /// keep up; admitting more writes will only deepen the stall").
    RocksDb = 3,
    /// Operator-initiated via `POST /admin/read-only/arm`. The auto
    /// watcher does NOT clear a manual arm — it stays read-only until
    /// `POST /admin/read-only/disarm`. This lets an operator hold the
    /// engine in a known state during maintenance windows without the
    /// guard fighting them.
    Manual = 4,
    /// Persist channel depth crossed the configured percentage of
    /// its capacity. Producer-side `persist_tx.send().await` is
    /// about to block for seconds-to-minutes waiting for a free
    /// slot — much better to return a clean 503 to the client now
    /// and let the SDK back off, than let it hang. v0.7.27.
    PersistQueue = 5,
}

impl ReadOnlyReason {
    /// Stable string identifier for logs, JSON bodies, and the
    /// Prometheus reason label. Changing these values is a breaking
    /// change for downstream alerting rules and SDK error handlers.
    pub fn as_str(self) -> &'static str {
        match self {
            ReadOnlyReason::None => "none",
            ReadOnlyReason::Memory => "memory",
            ReadOnlyReason::Disk => "disk",
            ReadOnlyReason::RocksDb => "rocksdb",
            ReadOnlyReason::Manual => "manual",
            ReadOnlyReason::PersistQueue => "persist_queue",
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => ReadOnlyReason::Memory,
            2 => ReadOnlyReason::Disk,
            3 => ReadOnlyReason::RocksDb,
            4 => ReadOnlyReason::Manual,
            5 => ReadOnlyReason::PersistQueue,
            _ => ReadOnlyReason::None,
        }
    }
}

/// Lock-free read-only flag.
///
/// Wrapped in an `Arc` inside `AppState`, so the same atomic is shared
/// across every cloned handler context, the resource-guard task, and
/// any helper that wants to gate a write at the call site (e.g. the
/// fee distributor checks this before producing a Reward block).
///
/// All operations are `Relaxed` — we only need atomicity, not a
/// happens-before relationship across the rest of the program.
/// A late observer reading "still armed" right after a disarm just
/// gets one extra 503; the next request sees the cleared state.
#[derive(Debug, Default)]
pub struct ReadOnlyMode {
    state: AtomicU8,
}

impl ReadOnlyMode {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(ReadOnlyReason::None as u8),
        }
    }

    /// Returns true when the engine is currently in read-only mode.
    pub fn is_armed(&self) -> bool {
        self.state.load(Ordering::Relaxed) != ReadOnlyReason::None as u8
    }

    /// Returns the current read-only reason (or `None` when clear).
    pub fn reason(&self) -> ReadOnlyReason {
        ReadOnlyReason::from_u8(self.state.load(Ordering::Relaxed))
    }

    /// Arm with a reason. Returns the previous reason (useful for
    /// transition logging — "was: memory, now: disk").
    /// Idempotent: arming with the same reason is a no-op.
    /// Passing `ReadOnlyReason::None` here is equivalent to `disarm()`.
    pub fn arm(&self, reason: ReadOnlyReason) -> ReadOnlyReason {
        let prev = self.state.swap(reason as u8, Ordering::Relaxed);
        ReadOnlyReason::from_u8(prev)
    }

    /// Disarm. Returns the previous reason.
    pub fn disarm(&self) -> ReadOnlyReason {
        let prev = self
            .state
            .swap(ReadOnlyReason::None as u8, Ordering::Relaxed);
        ReadOnlyReason::from_u8(prev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_is_not_armed() {
        let m = ReadOnlyMode::new();
        assert!(!m.is_armed());
        assert_eq!(m.reason(), ReadOnlyReason::None);
    }

    #[test]
    fn arm_disarm_cycle() {
        let m = ReadOnlyMode::new();
        let prev = m.arm(ReadOnlyReason::Memory);
        assert_eq!(prev, ReadOnlyReason::None);
        assert!(m.is_armed());
        assert_eq!(m.reason(), ReadOnlyReason::Memory);

        let prev = m.arm(ReadOnlyReason::Disk);
        assert_eq!(prev, ReadOnlyReason::Memory);
        assert_eq!(m.reason(), ReadOnlyReason::Disk);

        let prev = m.disarm();
        assert_eq!(prev, ReadOnlyReason::Disk);
        assert!(!m.is_armed());
    }

    #[test]
    fn reason_str_is_stable() {
        // These strings are part of the wire surface (Prometheus labels,
        // 503 JSON, /admin/read-only/status). Lock them down.
        assert_eq!(ReadOnlyReason::None.as_str(), "none");
        assert_eq!(ReadOnlyReason::Memory.as_str(), "memory");
        assert_eq!(ReadOnlyReason::Disk.as_str(), "disk");
        assert_eq!(ReadOnlyReason::RocksDb.as_str(), "rocksdb");
        assert_eq!(ReadOnlyReason::Manual.as_str(), "manual");
        assert_eq!(ReadOnlyReason::PersistQueue.as_str(), "persist_queue");
    }
}
