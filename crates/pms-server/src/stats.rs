use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Debug)]
pub struct Stats {
    pub persisted_ok: AtomicU64,
    pub persisted_dup: AtomicU64,
    pub persisted_err: AtomicU64,
    pub gossip_out_ok: AtomicU64,
    pub gossip_out_rej: AtomicU64,
    pub gossip_out_err: AtomicU64,
}

impl Stats {
    pub const fn new() -> Self {
        Self {
            persisted_ok: AtomicU64::new(0),
            persisted_dup: AtomicU64::new(0),
            persisted_err: AtomicU64::new(0),
            gossip_out_ok: AtomicU64::new(0),
            gossip_out_rej: AtomicU64::new(0),
            gossip_out_err: AtomicU64::new(0),
        }
    }

    pub fn snapshot(&self) -> (u64, u64, u64, u64, u64, u64) {
        (
            self.persisted_ok.load(Relaxed),
            self.persisted_dup.load(Relaxed),
            self.persisted_err.load(Relaxed),
            self.gossip_out_ok.load(Relaxed),
            self.gossip_out_rej.load(Relaxed),
            self.gossip_out_err.load(Relaxed),
        )
    }
}
