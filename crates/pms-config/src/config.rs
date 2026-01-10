use serde::Deserialize;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("{0}")]
    Any(#[from] anyhow::Error),
    #[error("config: {0}")]
    Cfg(#[from] config::ConfigError),
}

fn default_tip_limit() -> usize {
    200
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rocks {
    pub path: String,
    #[serde(default)]
    pub prefix: String, // ex: "testnet" | "mainnet"
    #[serde(default = "default_tip_limit")]
    pub tip_limit: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Network {
    pub mode: NetworkMode,     // "dev" | "testnet" | "mainnet"
    pub network_id: String,    // "pms-dev" | "pms-main"
    pub protocol_version: u32, // 1
}

#[derive(Debug, Clone, Deserialize)]
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct TlsConfig {
    pub cert_pem: String,       // chemin cert
    pub key_pem: String,        // chemin clé (PKCS#8 ou EC SEC1)
    pub ca_pem: Option<String>, // chemin CA root (pour client P2P)
    #[serde(default)]
    pub whitelist_fp256: Vec<String>, // empreintes SHA-256 autorisées (optionnel)
}

#[derive(Debug, Clone, Deserialize)]
pub struct P2pConfig {
    #[serde(default)]
    pub known_peers: String, // Comma separated list of peers env-friendly
    pub bind_addr: Option<String>,
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
}

#[derive(Deserialize, Clone, Debug)]
pub struct SecretSettings {
    pub node_identity_key_path: String,
    pub admin_wallet_file: String,
}

#[derive(Clone)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub api_addr: String, // ex "127.0.0.1:8080"
    pub tls: Option<TlsConfig>,
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

    // ═══════════════════════════════════════════════════════════════════════
    // Fee Distribution (Treasury Tax + Fee Sharing)
    // ═══════════════════════════════════════════════════════════════════════
    /// Percentage of fees going to treasury (admin wallets). Default: 15%
    #[serde(default = "default_treasury_fee_percent")]
    pub treasury_fee_percent: u8,
    /// Percentage of fees going to block creator. Default: 45%
    #[serde(default = "default_creator_fee_percent")]
    pub creator_fee_percent: u8,
    /// Percentage of fees going to parent block signers (split equally). Default: 40%
    #[serde(default = "default_parents_fee_percent")]
    pub parents_fee_percent: u8,

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
}

fn default_fee_ratio() -> String {
    "0.035".to_string()
}
fn default_base_fee() -> String {
    "0.001".to_string()
}
fn default_platform_fee_ratio() -> String {
    "0.45".to_string()
}

// Fee distribution defaults
fn default_treasury_fee_percent() -> u8 {
    15
}
fn default_creator_fee_percent() -> u8 {
    45
}
fn default_parents_fee_percent() -> u8 {
    40
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeePickMode {
    Uniform,
    RoundRobin,
}
