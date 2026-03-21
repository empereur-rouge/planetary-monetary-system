use dashmap::DashMap;
use super::ActivityResp;

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub(super) struct ActivityCacheKey {
    pub(super) address: String,
    pub(super) filter_type: Option<String>,
    pub(super) asset_id: Option<String>,
    pub(super) limit: usize,
    pub(super) after_ts: Option<i64>,
    pub(super) after_id: Option<String>,
}

/// In-memory cache for activity responses.
/// Uses DashMap for lock-free concurrent reads with per-address invalidation.
pub struct ActivityCache {
    entries: DashMap<ActivityCacheKey, (ActivityResp, std::time::Instant)>,
    ttl: std::time::Duration,
    max_entries: usize,
}

impl ActivityCache {
    pub fn new(max_entries: usize, ttl_secs: u64) -> Self {
        Self {
            entries: DashMap::new(),
            ttl: std::time::Duration::from_secs(ttl_secs),
            max_entries,
        }
    }

    pub(super) fn get(&self, key: &ActivityCacheKey) -> Option<ActivityResp> {
        let entry = self.entries.get(key)?;
        let (resp, created) = entry.value();
        if created.elapsed() < self.ttl {
            let cloned = resp.clone();
            drop(entry);
            Some(cloned)
        } else {
            drop(entry);
            self.entries.remove(key);
            None
        }
    }

    pub(super) fn put(&self, key: ActivityCacheKey, resp: ActivityResp) {
        if self.entries.len() >= self.max_entries {
            // Phase 1: remove all TTL-expired entries (cheap, no wasted data).
            self.entries
                .retain(|_, (_, created)| created.elapsed() < self.ttl);

            // Phase 2: if still over capacity, forcefully evict entries down to
            // 70 % of max.  DashMap iteration order is arbitrary — equivalent to
            // random eviction, which is acceptable for a cache.
            if self.entries.len() >= self.max_entries {
                let target = self.max_entries * 7 / 10;
                let excess = self.entries.len().saturating_sub(target);
                let mut removed = 0;
                self.entries.retain(|_, _| {
                    if removed >= excess {
                        return true;
                    }
                    removed += 1;
                    false
                });
            }
        }
        self.entries.insert(key, (resp, std::time::Instant::now()));
    }

    /// Invalidate all cache entries for a specific address.
    pub fn invalidate_address(&self, addr: &str) {
        self.entries.retain(|k, _| k.address != addr);
    }
}

// ═══════════════════════════════════════════════════════════════════
// GET /v1/wallet/{address}/activity
// ═══════════════════════════════════════════════════════════════════

