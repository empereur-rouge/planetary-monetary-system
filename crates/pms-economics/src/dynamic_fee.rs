use pms_types_economics::DynamicFeeInfo;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Instant;

/// Tracks block persistence timestamps to compute rolling TPS and fee multiplier.
///
/// Thread-safe: uses a `Mutex<VecDeque<Instant>>` internally.
/// The window is cleaned on every call, so stale entries are pruned automatically.
pub struct TpsTracker {
    timestamps: Mutex<VecDeque<Instant>>,
    window_secs: u64,
}

impl TpsTracker {
    /// Create a new TPS tracker with the given rolling window duration.
    pub fn new(window_secs: u64) -> Self {
        Self {
            timestamps: Mutex::new(VecDeque::with_capacity(4096)),
            window_secs: window_secs.max(1),
        }
    }

    /// Record a new block persistence timestamp (call after each successful block persist).
    pub fn record_block(&self) {
        let now = Instant::now();
        let mut ts = self.timestamps.lock().unwrap();
        ts.push_back(now);
        // Eagerly prune old entries to bound memory
        let cutoff = now - std::time::Duration::from_secs(self.window_secs);
        while ts.front().is_some_and(|&t| t < cutoff) {
            ts.pop_front();
        }
    }

    /// Compute current TPS (blocks in the rolling window / window duration).
    pub fn current_tps(&self) -> f64 {
        let now = Instant::now();
        let mut ts = self.timestamps.lock().unwrap();
        let cutoff = now - std::time::Duration::from_secs(self.window_secs);
        while ts.front().is_some_and(|&t| t < cutoff) {
            ts.pop_front();
        }
        ts.len() as f64 / self.window_secs as f64
    }

    /// Compute the fee multiplier based on current TPS vs target.
    ///
    /// Formula: `max(1.0, current_tps / target_tps)`, capped at `max_multiplier`.
    /// If target_tps is 0, returns 1.0 (no multiplier).
    pub fn fee_multiplier(&self, target_tps: u32, max_multiplier: f64) -> f64 {
        if target_tps == 0 {
            return 1.0;
        }
        let tps = self.current_tps();
        let raw = tps / target_tps as f64;
        raw.max(1.0).min(max_multiplier)
    }

    /// Get full dynamic fee info for diagnostics / API exposure.
    pub fn info(&self, target_tps: u32, max_multiplier: f64) -> DynamicFeeInfo {
        let tps = self.current_tps();
        let raw = if target_tps == 0 {
            1.0
        } else {
            (tps / target_tps as f64).max(1.0).min(max_multiplier)
        };
        DynamicFeeInfo {
            current_tps: tps,
            target_tps,
            multiplier: raw,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn empty_tracker_tps_is_zero() {
        let t = TpsTracker::new(60);
        let tps = t.current_tps();
        println!("tps={tps}");
        assert_eq!(tps, 0.0);
    }

    #[test]
    fn multiplier_defaults_to_1_when_idle() {
        let t = TpsTracker::new(60);
        let m = t.fee_multiplier(100, 5.0);
        println!("multiplier={m}");
        assert_eq!(m, 1.0);
    }

    #[test]
    fn multiplier_with_zero_target_returns_1() {
        let t = TpsTracker::new(60);
        t.record_block();
        let m = t.fee_multiplier(0, 5.0);
        println!("multiplier={m}");
        assert_eq!(m, 1.0);
    }

    #[test]
    fn tps_increases_with_blocks() {
        let t = TpsTracker::new(60);
        for _ in 0..120 {
            t.record_block();
        }
        let tps = t.current_tps();
        println!("tps={tps} (120 blocks in 60s window)");
        assert_eq!(tps, 2.0); // 120/60
    }

    #[test]
    fn multiplier_increases_under_load() {
        let t = TpsTracker::new(60);
        // Simulate 600 blocks (10 TPS) with target 5
        for _ in 0..600 {
            t.record_block();
        }
        let m = t.fee_multiplier(5, 10.0);
        println!("multiplier={m} (600 blocks / 60s = 10 TPS, target=5)");
        assert_eq!(m, 2.0); // 10/5 = 2.0
    }

    #[test]
    fn multiplier_capped_at_max() {
        let t = TpsTracker::new(60);
        // 3000 blocks = 50 TPS, target=5 → raw=10.0, capped at 5.0
        for _ in 0..3000 {
            t.record_block();
        }
        let m = t.fee_multiplier(5, 5.0);
        println!("multiplier={m} (should be capped at 5.0)");
        assert_eq!(m, 5.0);
    }

    #[test]
    fn old_entries_pruned() {
        let t = TpsTracker::new(1); // 1s window
        t.record_block();
        // Sleep just past the window
        std::thread::sleep(Duration::from_millis(1100));
        let tps = t.current_tps();
        println!("tps after expiry={tps}");
        assert_eq!(tps, 0.0);
    }

    #[test]
    fn info_returns_correct_data() {
        let t = TpsTracker::new(60);
        for _ in 0..300 {
            t.record_block();
        }
        let info = t.info(100, 5.0);
        println!("info: tps={} target={} mult={}", info.current_tps, info.target_tps, info.multiplier);
        assert_eq!(info.current_tps, 5.0);
        assert_eq!(info.target_tps, 100);
        assert_eq!(info.multiplier, 1.0); // 5/100 < 1.0, clamped to 1.0
    }
}
