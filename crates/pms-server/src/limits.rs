// server/limits.rs
pub const MAX_MSG_BYTES: usize = 10 * 1024 * 1024; // 10 MiB/message
pub const MAX_LINE_BYTES: usize = 10 * 1024 * 1024; // lecture JSONL
/// Taille du buffer pour la file de sortie par pair.
/// Si un pair ne lit pas assez vite, ses messages seront drop une fois la file pleine.
/// NOTE: Per-peer queue cap is now configurable via `[p2p] per_peer_queue_cap` in TOML.
/// This constant is kept as documentation of the default value.
pub const PER_PEER_Q_CAP: usize = 2_000;
pub const MAX_CONN_PER_IP: usize = 8; // connexions par IP

// Token bucket (messages/s) et burst
pub const RATE_MSGS_PER_SEC: u32 = 10_000; // Message par seconde (was 50)
pub const RATE_BURST: u32 = 20_000; // (was 100)

// Quotas "block" (anti flood)
pub const RATE_BLOCKS_PER_SEC: u32 = 5_000; // (was 10)
pub const RATE_BLOCK_BURST: u32 = 10_000; // (was 20)

// Cooldowns / ban
pub const TEMP_BAN_SECS: u64 = 30;
pub const HARD_BAN_THRESHOLD: u32 = 3; // récidives
pub const MAX_PARSE_ERRORS: u32 = 8; // au-delà → kick

// pms-server/src/limits.rs
pub const HANDSHAKE_TIMEOUT_MS: u64 = 1500;
pub const PING_EVERY_MS: u32 = 1000;

// Cache « vus »
pub const SEEN_TTL_MS: u64 = 5_000; // 5s (MVP)
pub const SEEN_CAPACITY: usize = 10_000; // borne mémoire

// bornes / quotas de rattrapage
pub const MAX_BLOCKS_BATCH: usize = 512; // (was 32)
/// NOTE: Max inflight is now configurable via `[p2p] max_inflight_requests` in TOML.
pub const MAX_INFLIGHT_GETBLOCK: usize = 10_000;
pub const INFLIGHT_TTL_MS: u128 = 10_000; // 10s TTL pour les requêtes en vol

// Orphan cache bounds (memory safety)
// NOTE: These are now configurable via `[p2p] max_orphans` / `max_parent_deps` in TOML.
pub const MAX_ORPHANS: usize = 2_000;
pub const MAX_PARENT_DEPS: usize = 5_000;
