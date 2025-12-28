use config::{Config, Environment, File};
use serde::Deserialize;
use std::path::PathBuf;
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
    pub wallet_addresses: Vec<String>, // liste Bech32m
    /// Liste des clés publiques ECDSA hex autorisées à signer les blocs de mint.
    /// Si vide → en dev on bypass le check, en prod tu pourras le rendre obligatoire.
    #[serde(default)]
    pub signer_pubkeys: Vec<String>,
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
    pub cert_pem: String, // chemin cert
    pub key_pem: String,  // chemin clé (PKCS#8 ou EC SEC1)
    #[serde(default)]
    pub whitelist_fp256: Vec<String>, // empreintes SHA-256 autorisées (optionnel)
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeesSettings {
    pub epsilon: String, // "0.001"
    pub scan_limit: u32, // ex 2000
    pub mode: FeePickMode,
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeePickMode {
    Uniform,
    RoundRobin,
}
