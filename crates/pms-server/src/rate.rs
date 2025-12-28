// server/rate.rs
use std::time::{Instant};

#[derive(Clone, Debug)]
pub struct TokenBucket {
    cap: u32, refill_per_sec: u32,
    tokens: u32, last: Instant,
}
impl TokenBucket {
    pub fn new(refill_per_sec: u32, burst: u32) -> Self {
        Self { cap: burst, refill_per_sec, tokens: burst, last: Instant::now() }
    }
    pub fn take(&mut self, n: u32) -> bool {
        // refill
        let now = Instant::now();
        let dt = now.saturating_duration_since(self.last);
        let add = (self.refill_per_sec as u128 * dt.as_millis() / 1000) as u32;
        if add > 0 {
            self.tokens = self.tokens.saturating_add(add).min(self.cap);
            self.last = now;
        }
        if self.tokens >= n { self.tokens -= n; true } else { false }
    }
}
