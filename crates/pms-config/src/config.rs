use serde::{Deserialize, Serialize};

use crate::runtime::FeeTier;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("{0}")]
    Any(#[from] anyhow::Error),
    #[error("config: {0}")]
    Cfg(#[from] config::ConfigError),
}

fn default_true() -> bool {
    true
}

fn default_tip_limit() -> usize {
    200
}

fn default_max_dag_blocks() -> usize {
    50_000
}

fn default_max_spent_outpoints() -> usize {
    500_000
}

fn default_max_utxos() -> usize {
    2_000_000
}

fn default_write_buffer_size_mb() -> usize {
    16
}

fn default_max_write_buffer_number() -> i32 {
    3
}

fn default_block_cache_size_mb() -> usize {
    512
}

fn default_db_write_buffer_size_mb() -> usize {
    512
}

fn default_max_open_files() -> i32 {
    512
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rocks {
    pub path: String,
    #[serde(default)]
    pub prefix: String, // ex: "testnet" | "mainnet"
    #[serde(default = "default_tip_limit")]
    pub tip_limit: usize,
    /// Maximum number of blocks kept in the in-memory DAG (ConcurrentDag).
    /// Older blocks are pruned to bound RAM usage. 0 = unlimited.
    /// Default: 50 000 (~50 MB RAM).
    #[serde(default = "default_max_dag_blocks")]
    pub max_dag_blocks: usize,
    /// Maximum spent outpoints tracked in RAM for double-spend detection.
    /// 0 = unlimited. Default: 500 000.
    #[serde(default = "default_max_spent_outpoints")]
    pub max_spent_outpoints: usize,
    /// Maximum UTXOs kept in the in-memory LRU cache (ShardedUtxoSet).
    /// On cache miss, falls back to RocksDB. 0 = unlimited (all UTXOs in RAM).
    /// Default: 2 000 000 (~64 MB RAM with CompactOutput ~32 bytes).
    #[serde(default = "default_max_utxos")]
    pub max_utxos: usize,
    /// Intervalle entre chaque backup (checkpoint) en secondes.
    /// Défaut: 21600 (6 heures).
    #[serde(default)]
    pub checkpoint_interval_secs: Option<u64>,

    /// Auto-backfill missing `activity_items` in background on startup.
    /// Processes blocks that have `addr_activity` entries but no pre-computed
    /// `activity_items`, ensuring 100% fast-path coverage for history queries.
    /// Idempotent and safe for production. Default: true.
    #[serde(default = "default_true")]
    pub auto_reindex_activity_items: bool,

    // ── RocksDB memory tuning ──────────────────────────────────────────

    /// Write buffer (memtable) size per column family, in MB.
    /// Each CF can hold up to `max_write_buffer_number` memtables of this size.
    /// CRITICAL: Total memtable RAM = num_CFs × max_write_buffer_number × this value.
    /// With 2 ledgers (67 CFs), 16 MB × 3 = 3.2 GiB max; 32 MB × 3 = 6.4 GiB (OOM risk).
    /// Default: 16 MB (safe for multi-ledger on 16 GB VPS).
    #[serde(default = "default_write_buffer_size_mb")]
    pub write_buffer_size_mb: usize,

    /// Maximum number of write buffers (memtables) kept in memory per CF.
    /// Higher values absorb write bursts but increase memory. Default: 3.
    #[serde(default = "default_max_write_buffer_number")]
    pub max_write_buffer_number: i32,

    /// Shared LRU block cache size in MB, shared across ALL column families.
    /// Holds data blocks, index blocks, and filter blocks.
    /// With Direct I/O (v0.5.21), this is the ONLY read cache.
    /// Default: 512 MB. Scale with available RAM after memtable budget.
    #[serde(default = "default_block_cache_size_mb")]
    pub block_cache_size_mb: usize,

    /// Global memtable memory budget in MB (`db_write_buffer_size`).
    /// Caps the TOTAL memory used by ALL memtables across ALL column families.
    /// Critical for multi-ledger setups where N ledgers × 33 CFs can spike.
    /// 0 = disabled (each CF manages independently — can OOM with many CFs).
    /// Recommended: 512 MB for 8 GB VPS with multiple ledgers.
    /// Default: 512 MB.
    #[serde(default = "default_db_write_buffer_size_mb")]
    pub db_write_buffer_size_mb: usize,

    /// Maximum number of open file descriptors RocksDB may use.
    /// Each SSTable consumes one FD. With 66+ CFs and deep LSM trees,
    /// RocksDB can easily exceed the OS ulimit (typically 1024 on Linux).
    /// -1 = unlimited (RocksDB default, dangerous on VPS).
    /// Recommended: 512 for 8 GB VPS. Default: 512.
    #[serde(default = "default_max_open_files")]
    pub max_open_files: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Network {
    pub mode: NetworkMode,     // "dev" | "testnet" | "mainnet"
    pub network_id: String,    // "pms-dev" | "pms-main"
    pub protocol_version: u32, // 1
    /// Native token symbol (default: "PMS")
    #[serde(default)]
    pub symbol: Option<String>,
}

/// The Public Key of the Coordinator (Master Node) for Mainnet.
/// Blocks signed by this key are treated as Milestones/Checkpoints.
pub const COORDINATOR_PUBLIC_KEY_MAINNET: &str =
    "036ed4d5ad1c927fe972ef9728ac1888d237af57a488b6cbe50228fac442b5ae6b";

/// The Public Key of the Coordinator for Testnet.
pub const COORDINATOR_PUBLIC_KEY_TESTNET: &str =
    "02115e0941c01a05f6d6dfc6aa9204e20d0d1af9d3231c25728034d9278bf7187f";

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    Dev,
    Testnet,
    Mainnet,
}

impl NetworkMode {
    pub fn is_prod(&self) -> bool {
        matches!(self, NetworkMode::Mainnet)
    }
    pub fn is_non_prod(&self) -> bool {
        !self.is_prod()
    }
    /// Returns the hardcoded coordinator public key for this network mode.
    /// Returns `None` for Dev mode (no coordinator enforcement).
    pub fn coordinator_public_key(&self) -> Option<&'static str> {
        match self {
            NetworkMode::Mainnet => Some(COORDINATOR_PUBLIC_KEY_MAINNET),
            NetworkMode::Testnet => Some(COORDINATOR_PUBLIC_KEY_TESTNET),
            NetworkMode::Dev => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Address {
    pub hrp: String, // ex: "8e"
}

#[derive(Debug, Clone, Deserialize)]
pub struct Admin {
    #[serde(default)]
    pub wallet_addresses: Vec<String>, // liste Bech32m (obsolète, voir treasury_wallets_file)
    /// Liste des clés publiques ECDSA hex autorisées à signer les blocs de mint.
    /// Si vide → en dev on bypass le check, en prod tu pourras le rendre obligatoire.
    #[serde(default)]
    pub signer_pubkeys: Vec<String>,
    /// Chemin vers le fichier JSON des wallets Treasury signés par le Coordinator
    #[serde(default)]
    pub treasury_wallets_file: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Client {
    pub bind_addr: String, // p2p listener
    pub api_addr: String,  // API interne (derrière proxy en prod)
    #[serde(default)]
    pub allow_insecure_tls: bool, // true en dev/testnet, false en mainnet
    #[serde(default)]
    pub internal_api_addr: Option<String>, // Internal API for Gateway (ex: "0.0.0.0:3000")
    /// Controls whether the HTTP API (port 8080) uses TLS.
    /// When `false`, the API serves plain HTTP even if `[tls]` is configured.
    /// P2P always uses its own TLS from `[tls]` section independently.
    /// Default: `true` (backward compatible — API uses TLS if `[tls]` is present).
    /// Set to `false` when the engine runs behind a gateway on an internal Docker network.
    #[serde(default = "default_true")]
    pub api_tls_enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TlsConfig {
    pub cert_pem: String,       // chemin cert
    pub key_pem: String,        // chemin clé (PKCS#8 ou EC SEC1)
    pub ca_pem: Option<String>, // chemin CA root (pour client P2P)
    #[serde(default)]
    pub whitelist_fp256: Vec<String>, // empreintes SHA-256 autorisées (optionnel)
}

// ── P2P limit defaults ──────────────────────────────────────────────────

fn default_max_connections() -> usize {
    256
}
fn default_per_peer_queue_cap() -> usize {
    2_000
}
fn default_max_orphans() -> usize {
    2_000
}
fn default_max_inflight_requests() -> usize {
    10_000
}
fn default_max_parent_deps() -> usize {
    5_000
}
fn default_max_peer_retries() -> u32 {
    20
}

/// P2P network configuration.
///
/// Includes connectivity settings (`known_peers`, `allowed_peer_ips`) and
/// resource limits that control memory usage and scaling behaviour.
/// All limit fields have safe defaults tuned for an 8 GB VPS.
/// Increase them for vertical scaling (more RAM) or horizontal scaling (more peers).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct P2pConfig {
    #[serde(default)]
    pub known_peers: String,
    pub bind_addr: Option<String>,
    #[serde(default)]
    pub allowed_peer_ips: Vec<String>,
    #[serde(default)]
    pub strict_whitelist: bool,

    // ── Scaling limits (configurable since v0.5.9) ─────────────────────

    /// Maximum concurrent inbound P2P peer connections.
    /// Each connection uses ~2 tokio tasks + a per-peer queue.
    /// Increase for horizontal scaling (more peers). Default: 256.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,

    /// Per-peer outbound message queue capacity.
    /// Slow peers that exceed this limit will have messages dropped.
    /// Increase for vertical scaling (more RAM). Default: 2 000.
    #[serde(default = "default_per_peer_queue_cap")]
    pub per_peer_queue_cap: usize,

    /// Maximum orphan blocks held in memory waiting for missing parents.
    /// Higher values improve sync speed at the cost of RAM.
    /// Default: 2 000 (~2 MB).
    #[serde(default = "default_max_orphans")]
    pub max_orphans: usize,

    /// Maximum in-flight GetBlock requests tracked concurrently.
    /// Prevents unbounded memory growth from unanswered requests.
    /// Default: 10 000.
    #[serde(default = "default_max_inflight_requests")]
    pub max_inflight_requests: usize,

    /// Maximum parent→children dependency entries for orphan resolution.
    /// Limits memory used by the orphan dependency tracker.
    /// Default: 5 000.
    #[serde(default = "default_max_parent_deps")]
    pub max_parent_deps: usize,

    /// Maximum connection retry attempts per known peer at startup.
    /// Uses exponential backoff (5s → 60s cap). After max retries, gives up.
    /// Increase if peers are slow to boot. Default: 20.
    #[serde(default = "default_max_peer_retries")]
    pub max_peer_retries: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Limits {
    pub max_body_bytes: usize,   // 262144
    pub request_timeout_ms: u64, // 4000
    pub rate_limit_rps: u32,     // 20
    pub burst: u32,              // 40
}

#[derive(Debug, Clone, Deserialize)]
pub struct Auth {
    #[serde(default)]
    pub require_signed_submit: bool,
    /// Valeur "env:VAR_NAME" supportée
    pub admin_api_token: Option<String>,
    /// Liste des IPs/CIDR autorisées pour les routes admin (/metrics, /admin/*)
    /// Vide = autorise tout (dev), rempli = whitelist stricte (prod)
    /// Ex: ["192.168.1.0/24", "10.0.0.5/32"]
    #[serde(default)]
    pub allowed_ips: Vec<String>,
    /// Chemin vers le fichier JSON des clés API SDK.
    /// Si None → pas de vérification API key (mode dev, backward-compatible).
    /// Ex: "etc/pms/api-keys.json"
    #[serde(default)]
    pub api_keys_file: Option<String>,
}

/// `/healthz` thresholds. Defaults are deliberately permissive so a
/// dev box doesn't flap. Tighten in production: a healthy coordinator
/// emits a block at least every few seconds, and you want disk-free
/// alerts long before the FS fills up.
#[derive(Deserialize, Clone, Debug)]
pub struct HealthSettings {
    /// `/healthz` returns `degraded` when the most recent block is older
    /// than this. `0` disables the check. Default: `300` (5 min) — large
    /// enough not to flap on dev / quiet testnet, but still surfaces a
    /// coordinator that has stopped persisting on a busy network.
    #[serde(default = "default_max_last_block_age_seconds")]
    pub max_last_block_age_seconds: u64,
    /// `/healthz` returns `degraded` if the persist channel queue is at
    /// or above this fraction (0.0 – 1.0) of its capacity. Saturation
    /// means the persist pipeline is back-pressuring callers (cf. C2
    /// fix in v0.7.2). Default: `0.8`.
    #[serde(default = "default_persist_queue_high_water")]
    pub persist_queue_high_water: f64,
    /// `/healthz` returns `degraded` when the filesystem holding the
    /// RocksDB directory has less free space than this percentage.
    /// Default: `10.0` (i.e. flag at 10% free).
    #[serde(default = "default_min_disk_free_percent")]
    pub min_disk_free_percent: f64,
}

impl Default for HealthSettings {
    fn default() -> Self {
        Self {
            max_last_block_age_seconds: default_max_last_block_age_seconds(),
            persist_queue_high_water: default_persist_queue_high_water(),
            min_disk_free_percent: default_min_disk_free_percent(),
        }
    }
}

fn default_max_last_block_age_seconds() -> u64 {
    300
}
fn default_persist_queue_high_water() -> f64 {
    0.8
}
fn default_min_disk_free_percent() -> f64 {
    10.0
}

#[derive(Deserialize, Clone, Debug)]
pub struct SecretSettings {
    pub node_identity_key_path: String,
    pub admin_wallet_file: Option<String>,
    /// Optional AES-256-GCM encrypted coordinator key envelope produced by
    /// `tools-cli encrypt-coordinator-key`. When set AND the file exists,
    /// the server loads the node identity from it and reads the passphrase
    /// from the `PMS_COORDINATOR_KEY_PASSPHRASE` environment variable.
    /// Falls back to `node_identity_key_path` (plain hex) if absent —
    /// keeps existing dev/testnet deployments working without changes.
    /// See audit finding H-key and `pms-wallet::key_encryption`.
    #[serde(default)]
    pub node_identity_key_encrypted_path: Option<String>,
    /// If true, boot aborts when the key file has group/world permissions
    /// (`mode & 0o077 != 0`). Default: `false` (warning-only) so existing
    /// dev setups don't break. Flip to `true` in production.
    #[serde(default)]
    pub strict_key_permissions: bool,
}

/// Runtime configuration passed to the P2P server and API server.
///
/// `tls` controls P2P TLS. `api_tls_enabled` controls whether the HTTP API
/// also uses TLS (from the same `[tls]` section). When `api_tls_enabled` is
/// `false`, the API serves plain HTTP even if `[tls]` is configured — useful
/// when the engine runs behind a gateway on an internal Docker network.
#[derive(Clone)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub api_addr: String, // ex "127.0.0.1:8080"
    pub tls: Option<TlsConfig>,
    /// When `false`, API serves plain HTTP regardless of `[tls]`.
    /// P2P TLS is unaffected. Default: `true`.
    pub api_tls_enabled: bool,
    pub network: Network,
    pub auth: Auth,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValidationSettings {
    /// bits de difficulté PoW (nombre de bits à zéro en tête du hash)
    pub min_pow_leading_zero_bits: u8,
    pub max_payload_bytes: usize,
    pub min_parents_after_boot: usize,
    pub max_parents: usize,
    pub require_unique_parents: bool,
    pub forbid_self_parent: bool,
    pub max_inputs: usize,
    pub max_outputs: usize,
    pub max_tx_bytes: usize,
    pub max_fee_per_tx: usize,
    pub enforce_parent_existence: bool,
    pub enforce_fee_recipient: bool,
    #[serde(default)]
    pub allowed_fee_addresses: Vec<String>,
    pub coordinator_public_key: Option<String>, // Pour Dev/Testnet custom
    pub coordinator_x25519_public_key: Option<String>, // Clé chiffrement du Coordinator
    #[serde(default)]
    pub coordinator_tx_only: bool, // If true, node rejects non-privileged TXs

    // ═══════════════════════════════════════════════════════════════════════
    // Single Writer Mode (Private DAG)
    // ═══════════════════════════════════════════════════════════════════════
    /// En mode Single Writer, seul le Coordinator peut créer des blocs.
    /// - Tous les blocs doivent être signés par `coordinator_public_key`.
    /// - La chaîne est linéaire (1 parent par bloc, sauf genesis).
    /// - Désactive la logique multi-writer (orphelins, conflits, k-depth finality).
    ///
    /// Défaut: true (activé) - pour le mode Private DAG centralisé.
    /// Mettre à false uniquement si vous voulez activer le mode multi-writer.
    #[serde(default = "default_enforce_single_writer")]
    pub enforce_single_writer: bool,
}

// ═══════════════════════════════════════════════════════════════════════
// Fee Distribution N-Way
// ═══════════════════════════════════════════════════════════════════════

/// Un bénéficiaire dans le split N-way des fees.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeeBeneficiary {
    /// Rôle du bénéficiaire (ex: "coordinator", "treasury", "client", "partner")
    pub role: String,
    /// Part en basis points (0-10000 = 0-100%). Ex: 5000 = 50%, 3333 = 33.33%
    pub percent_bps: u16,
    /// Adresse pour recevoir la part. None = résolu au runtime
    /// ("coordinator" → node_wallet, "treasury" → random parmi treasury_addresses)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// Configuration N-way pour la distribution des fees.
/// Les basis points doivent totaliser 10000 (100%).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeeDistributionConfig {
    pub beneficiaries: Vec<FeeBeneficiary>,
}

impl Default for FeeDistributionConfig {
    fn default() -> Self {
        Self {
            beneficiaries: vec![
                FeeBeneficiary {
                    role: "coordinator".into(),
                    percent_bps: 6500,
                    address: None,
                },
                FeeBeneficiary {
                    role: "treasury".into(),
                    percent_bps: 3500,
                    address: None,
                },
            ],
        }
    }
}

impl FeeDistributionConfig {
    /// Constructeur pour un split 2-way coordinator/treasury.
    /// Les valeurs sont en basis points (ex: 6500 = 65%, 3500 = 35%).
    pub fn new(coordinator_bps: u16, treasury_bps: u16) -> Self {
        Self {
            beneficiaries: vec![
                FeeBeneficiary {
                    role: "coordinator".into(),
                    percent_bps: coordinator_bps,
                    address: None,
                },
                FeeBeneficiary {
                    role: "treasury".into(),
                    percent_bps: treasury_bps,
                    address: None,
                },
            ],
        }
    }

    /// Valide que les basis points totalisent 10000 (100%).
    pub fn validate(&self) -> Result<(), String> {
        let total: u32 = self
            .beneficiaries
            .iter()
            .map(|b| b.percent_bps as u32)
            .sum();
        if total != 10000 {
            return Err(format!(
                "Fee basis points must sum to 10000 (100%), got {}",
                total
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeesSettings {
    pub epsilon: String, // "0.001"
    #[serde(default = "default_fee_ratio")]
    pub ratio: String, // "0.035"
    #[serde(default = "default_base_fee")]
    pub base_fee: String, // "0.0"
    pub mode: FeePickMode,
    pub seed: Option<u64>,
    pub platform_address: Option<String>,
    pub platform_address_signature: Option<String>,
    #[serde(default = "default_platform_fee_ratio")]
    pub platform_fee_ratio: String, // ex: "0.02" pour 2%

    /// Barème de fees par paliers. Si non vide, remplace ratio pour le calcul.
    #[serde(default)]
    pub fee_tiers: Vec<crate::FeeTier>,

    /// Adresses des wallets de la trésorerie pour recevoir les frais (taxe).
    #[serde(default)]
    pub treasury_addresses: Vec<String>,

    // ═══════════════════════════════════════════════════════════════════════
    // Fee Distribution (Coordinator + Treasury)
    // ═══════════════════════════════════════════════════════════════════════
    /// Percentage of fees going to treasury wallets. Default: 35%
    #[serde(default = "default_treasury_fee_percent")]
    pub treasury_fee_percent: u8,
    /// Percentage of fees going to coordinator. Default: 65%
    #[serde(default = "default_coordinator_fee_percent")]
    pub coordinator_fee_percent: u8,

    /// Distribution N-way des fees. Si None, utilise coordinator_fee_percent + treasury_fee_percent.
    #[serde(default)]
    pub fee_distribution: Option<FeeDistributionConfig>,

    // ═══════════════════════════════════════════════════════════════════════
    // Mint / Token / NFT Fees
    // ═══════════════════════════════════════════════════════════════════════
    /// Frais fixe sur le minting de tokens custom. Default: None (pas de mint fee)
    #[serde(default)]
    pub mint_fee_base: Option<String>,
    /// Ratio sur le montant minté. Default: None
    #[serde(default)]
    pub mint_fee_ratio: Option<String>,
    /// Fee one-time pour la création de token. Default: None
    #[serde(default)]
    pub token_creation_fee: Option<String>,
    /// Fee sur le mint de NFT. Default: None
    #[serde(default)]
    pub nft_mint_fee: Option<String>,
    /// Types de NFT exemptés de fee (ex: ["cube", "reward"])
    #[serde(default)]
    pub nft_fee_exempt_types: Vec<String>,

    // ═══════════════════════════════════════════════════════════════════════
    // Block Rewards (Inflation)
    // ═══════════════════════════════════════════════════════════════════════
    /// Base reward per block (e.g., "0.1" PMS). Set to "0" to disable.
    #[serde(default = "default_block_reward")]
    pub block_reward: String,
    /// Annual inflation rate percentage. Default: 3.0%
    #[serde(default = "default_annual_inflation_percent")]
    pub annual_inflation_percent: f64,
    /// Percentage of block reward to treasury. Default: 20%
    #[serde(default = "default_treasury_reward_percent")]
    pub treasury_reward_percent: u8,
    /// Percentage of block reward to block creator. Default: 70%
    #[serde(default = "default_creator_reward_percent")]
    pub creator_reward_percent: u8,
    /// Percentage of block reward to burn (deflationary pressure). Default: 10%
    #[serde(default = "default_burn_percent")]
    pub burn_percent: u8,

    // ═══════════════════════════════════════════════════════════════════════
    // Economics — Fee Burn, Gas Pool, Dynamic Fees, Storage Fees
    // ═══════════════════════════════════════════════════════════════════════
    /// Percentage of tx fees permanently burned (basis points). 3000 = 30%. Default: 0 (disabled).
    #[serde(default)]
    pub burn_rate_bps: u32,
    /// Gas cost per transaction on custom ledgers (deducted from ledger's gas pool). Default: None (disabled).
    #[serde(default)]
    pub gas_per_tx: Option<String>,
    /// Minimum gas pool balance before ledger goes read-only. Default: None.
    #[serde(default)]
    pub gas_pool_min_balance: Option<String>,
    /// Fee for deploying/registering a smart contract. Default: None.
    #[serde(default)]
    pub contract_deployment_fee: Option<String>,
    /// Storage fee per KB of payload data. Default: None (disabled).
    #[serde(default)]
    pub storage_fee_per_kb: Option<String>,
    /// Enable congestion-based dynamic fee multiplier. Default: false.
    #[serde(default)]
    pub dynamic_fee_enabled: bool,
    /// Target TPS for dynamic fee calculation. Fees increase above this. Default: 100.
    #[serde(default = "default_target_tps")]
    pub target_tps: u32,
    /// Maximum fee multiplier under congestion. Default: 5.0.
    #[serde(default = "default_max_fee_multiplier")]
    pub max_fee_multiplier: f64,
    /// Fee multiplier for cross-ledger bridge transfers. Default: 2.0.
    #[serde(default = "default_cross_ledger_fee_multiplier")]
    pub cross_ledger_fee_multiplier: f64,
    /// Annual subscription fee (PMS) for custom ledgers. Default: None.
    #[serde(default)]
    pub ledger_annual_fee_pms: Option<String>,

    // ═══════════════════════════════════════════════════════════════════════
    // Automated Distribution
    // ═══════════════════════════════════════════════════════════════════════
    /// Interval in seconds for automated fee distribution. Default: 600 (10 minutes).
    #[serde(default = "default_distribution_interval_sec")]
    pub distribution_interval_sec: u64,

    // ═══════════════════════════════════════════════════════════════════════
    // Scheduled Inflation Mint
    // ═══════════════════════════════════════════════════════════════════════
    /// Enable daily scheduled inflation mint. Default: false.
    #[serde(default)]
    pub daily_inflation_enabled: bool,
    /// Interval in seconds for scheduled inflation mint. Default: 86400 (24h).
    #[serde(default = "default_daily_inflation_interval_sec")]
    pub daily_inflation_interval_sec: u64,
}

fn default_fee_ratio() -> String {
    "0.035".to_string()
}
fn default_base_fee() -> String {
    "0.0000001".to_string()
}
fn default_platform_fee_ratio() -> String {
    "0.45".to_string()
}
fn default_distribution_interval_sec() -> u64 {
    600
}
fn default_daily_inflation_interval_sec() -> u64 {
    86400 // 24 hours
}

// Fee distribution defaults (coordinator + treasury = 100%)
fn default_treasury_fee_percent() -> u8 {
    35
}
fn default_coordinator_fee_percent() -> u8 {
    65
}

// Block reward defaults
fn default_block_reward() -> String {
    "0.1".to_string()
}
fn default_annual_inflation_percent() -> f64 {
    3.0
}
fn default_treasury_reward_percent() -> u8 {
    20
}
fn default_creator_reward_percent() -> u8 {
    70
}
fn default_burn_percent() -> u8 {
    10
}

// Single Writer Mode default (Private DAG)
fn default_enforce_single_writer() -> bool {
    true // Par défaut, seul le Coordinator peut écrire des blocs
}

// Economics defaults
fn default_target_tps() -> u32 {
    100
}
fn default_max_fee_multiplier() -> f64 {
    5.0
}
fn default_cross_ledger_fee_multiplier() -> f64 {
    2.0
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeePickMode {
    Uniform,
    RoundRobin,
}

// ═══════════════════════════════════════════════════════════════════════
// Multi-Ledger Configuration
// ═══════════════════════════════════════════════════════════════════════

/// Configuration d'un ledger individuel.
/// Chaque ledger a son propre prefix RocksDB, network_id, et éventuellement
/// des settings de validation/fees spécifiques.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerDef {
    /// Identifiant unique du ledger (ex: "main", "nft", "client-acme")
    pub id: String,
    /// Network ID pour le protocole P2P (ex: "pms-main", "pms-nft")
    pub network_id: String,
    /// Prefix RocksDB pour isoler les données (ex: "pms:main", "nft")
    pub prefix: String,
    /// Version du protocole P2P
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
    /// Tip limit override (sinon hérite du global)
    #[serde(default)]
    pub tip_limit: Option<usize>,
    /// Fee settings override pour ce ledger
    #[serde(default)]
    pub fees: Option<LedgerFeesOverride>,
    /// Validation settings override pour ce ledger
    #[serde(default)]
    pub validation: Option<LedgerValidationOverride>,
    /// Clé publique (Ed25519) du propriétaire du ledger.
    /// None = admin-owned (ex: "main"), Some = custom ledger avec owner.
    #[serde(default)]
    pub owner_pubkey: Option<String>,
    /// Clé publique X25519 du propriétaire (pour chiffrement).
    /// Utilisée pour chiffrer les blocs de transfert d'ownership dans le DAG.
    #[serde(default)]
    pub owner_x25519_pubkey: Option<String>,
    /// Native token symbol for this ledger (default: "PMS")
    #[serde(default)]
    pub symbol: Option<String>,
}

/// Overrides de fees pour un ledger spécifique.
/// Chaque champ à `None` hérite de la config globale `FeesSettings`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LedgerFeesOverride {
    pub ratio: Option<String>,
    pub base_fee: Option<String>,
    pub platform_fee_ratio: Option<String>,
    pub block_reward: Option<String>,
    #[serde(default)]
    pub fee_tiers: Vec<FeeTier>,
    #[serde(default)]
    pub mint_fee_base: Option<String>,
    #[serde(default)]
    pub mint_fee_ratio: Option<String>,
    #[serde(default)]
    pub token_creation_fee: Option<String>,
    #[serde(default)]
    pub nft_mint_fee: Option<String>,
    #[serde(default)]
    pub nft_fee_exempt_types: Vec<String>,
    #[serde(default)]
    pub fee_distribution: Option<FeeDistributionConfig>,
    #[serde(default)]
    pub treasury_fee_percent: Option<u8>,
    #[serde(default)]
    pub coordinator_fee_percent: Option<u8>,

    // Economics overrides
    #[serde(default)]
    pub burn_rate_bps: Option<u32>,
    #[serde(default)]
    pub gas_per_tx: Option<String>,
    #[serde(default)]
    pub gas_pool_min_balance: Option<String>,
    #[serde(default)]
    pub contract_deployment_fee: Option<String>,
    #[serde(default)]
    pub storage_fee_per_kb: Option<String>,
}

/// Overrides de validation pour un ledger spécifique.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LedgerValidationOverride {
    pub max_inputs: Option<usize>,
    pub max_outputs: Option<usize>,
    pub max_tx_bytes: Option<usize>,
    pub enforce_single_writer: Option<bool>,
}

fn default_protocol_version() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_fees_override_deserializes_with_all_new_fields() {
        let json = serde_json::json!({
            "ratio": "0.01",
            "base_fee": "0.5",
            "platform_fee_ratio": "0.10",
            "block_reward": "0.2",
            "mint_fee_base": "2.0",
            "mint_fee_ratio": "0.03",
            "token_creation_fee": "200",
            "nft_mint_fee": "1.0",
            "nft_fee_exempt_types": ["cube", "reward"],
            "treasury_fee_percent": 20,
            "coordinator_fee_percent": 80,
            "fee_tiers": [
                { "up_to": "100", "ratio": "0.03" },
                { "ratio": "0.01" }
            ]
        });

        let ov: LedgerFeesOverride = serde_json::from_value(json).expect("should parse");
        assert_eq!(ov.ratio, Some("0.01".into()));
        assert_eq!(ov.mint_fee_base, Some("2.0".into()));
        assert_eq!(ov.token_creation_fee, Some("200".into()));
        assert_eq!(ov.nft_mint_fee, Some("1.0".into()));
        assert_eq!(ov.nft_fee_exempt_types, vec!["cube", "reward"]);
        assert_eq!(ov.fee_tiers.len(), 2);
        assert_eq!(ov.treasury_fee_percent, Some(20));
        assert_eq!(ov.coordinator_fee_percent, Some(80));
    }

    #[test]
    fn ledger_fees_override_backward_compat_minimal() {
        // Old JSON with only original fields → new fields default
        let json = serde_json::json!({
            "ratio": "0.01",
            "base_fee": "0.5"
        });

        let ov: LedgerFeesOverride = serde_json::from_value(json).expect("should parse");
        assert_eq!(ov.ratio, Some("0.01".into()));
        assert_eq!(ov.base_fee, Some("0.5".into()));
        // New fields should all be default/None/empty
        assert!(ov.mint_fee_base.is_none());
        assert!(ov.mint_fee_ratio.is_none());
        assert!(ov.token_creation_fee.is_none());
        assert!(ov.nft_mint_fee.is_none());
        assert!(ov.nft_fee_exempt_types.is_empty());
        assert!(ov.fee_tiers.is_empty());
        assert!(ov.fee_distribution.is_none());
        assert!(ov.treasury_fee_percent.is_none());
        assert!(ov.coordinator_fee_percent.is_none());
    }

    #[test]
    fn fee_distribution_config_roundtrip() {
        let config = FeeDistributionConfig::new(7000, 3000);
        assert!(config.validate().is_ok());
        assert_eq!(config.beneficiaries.len(), 2);
        assert_eq!(config.beneficiaries[0].role, "coordinator");
        assert_eq!(config.beneficiaries[0].percent_bps, 7000);
        assert_eq!(config.beneficiaries[1].role, "treasury");
        assert_eq!(config.beneficiaries[1].percent_bps, 3000);
    }
}
