use crate::{
    Address, Admin, Auth, Client, FeesSettings, LedgerDef, Limits, LoadError, Network, NetworkMode,
    P2pConfig, Rocks, SecretSettings, TlsConfig, ValidationSettings,
};
use anyhow::{Result, bail};

use config::{Config, ConfigError, Environment, File};
use serde::Deserialize;
use std::env;
use std::path::{Path, PathBuf};

const ROOT_CONFIG_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config");

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub rocks: Rocks,
    pub network: Network,
    pub address: Address,
    pub admin: Admin,
    pub client: Option<Client>,
    pub tls: Option<TlsConfig>,
    pub limits: Limits,
    pub auth: Auth,
    pub secrets: SecretSettings,
    pub validation: ValidationSettings,
    pub fees: FeesSettings,
    pub p2p: P2pConfig,
    /// Multi-ledger definitions. Si absent, un seul ledger "main" est créé
    /// automatiquement à partir de [rocks] et [network].
    #[serde(default)]
    pub ledgers: Vec<LedgerDef>,
}

impl Settings {
    /// Retourne les définitions de ledgers effectives.
    /// Si aucun `[[ledgers]]` n'est défini dans la config, génère automatiquement
    /// un seul ledger "main" à partir de [rocks] et [network] (rétrocompatibilité).
    pub fn effective_ledgers(&self) -> Vec<LedgerDef> {
        if self.ledgers.is_empty() {
            vec![LedgerDef {
                id: "main".to_string(),
                network_id: self.network.network_id.clone(),
                prefix: self.rocks.prefix.clone(),
                protocol_version: self.network.protocol_version,
                tip_limit: Some(self.rocks.tip_limit),
                fees: None,
                validation: None,
                owner_pubkey: None,
                symbol: self.network.symbol.clone(),
            }]
        } else {
            self.ledgers.clone()
        }
    }

    pub fn validate(&self) -> Result<()> {
        // 1) Prefix attendu selon le mode
        let expected_prefix = match self.network.mode {
            NetworkMode::Dev => "pms:dev",
            NetworkMode::Testnet => "pms:test",
            NetworkMode::Mainnet => "pms:main",
        };
        if self.rocks.prefix != expected_prefix {
            bail!(
                "Prefix incohérent pour {:?}: attendu '{}', reçu '{}'",
                self.network.mode,
                expected_prefix,
                self.rocks.prefix
            );
        }

        // 2) Client insecure TLS interdit en prod
        // NOTE: On utilise if-let combiné avec && pour satisfaire clippy::collapsible_if
        if self.network.mode.is_prod()
            && let Some(c) = &self.client
            && c.allow_insecure_tls {
                bail!("Mainnet: client.allow_insecure_tls doit être false");
            }
            // Note: api_addr est une adresse de bind (ex: 0.0.0.0:8080), pas une URL.
            // La sécurité HTTPS est assurée par le bloc [tls] obligatoire en mainnet.

        // 3) TLS
        if let Some(tls) = &self.tls {
            if self.network.mode.is_prod() {
                if !Path::new(&tls.cert_pem).exists() {
                    bail!("TLS: fichier introuvable: {}", tls.cert_pem);
                }
                if !Path::new(&tls.key_pem).exists() {
                    bail!("TLS: fichier introuvable: {}", tls.key_pem);
                }
            // NOTE: else if au lieu de else { if } pour satisfaire clippy::collapsible_else_if
            } else if !Path::new(&tls.cert_pem).exists() || !Path::new(&tls.key_pem).exists() {
                eprintln!(
                    "[config] ⚠ TLS files not found for mode {:?}, check ignoré (non-prod)",
                    self.network.mode
                );
            }
        } else if self.network.mode.is_prod() {
            bail!("TLS: bloc [tls] obligatoire en mainnet");
        }

        // 4) 🔐 Secrets (node_identity_key_path + admin_wallet_file)
        {
            let secrets = &self.secrets;

            let node_key = Path::new(&secrets.node_identity_key_path);

            // Check node key
            if !node_key.exists() && self.network.mode.is_prod() {
                bail!(
                    "Secrets manquants en prod : node_identity_key_path='{}'",
                    secrets.node_identity_key_path
                );
            }

            // Check admin wallet ONLY if specified
            if let Some(path) = &secrets.admin_wallet_file {
                let admin_file = Path::new(path);
                if !admin_file.exists() && self.network.mode.is_prod() {
                    bail!("Secrets manquants en prod : admin_wallet_file='{}'", path);
                }
            }
        }

        // 5) Mining power validation
        if self.validation.min_pow_leading_zero_bits > 32 {
            bail!("validation.min_pow_leading_zero_bits > 32 est absurde");
        }

        Ok(())
    }
}

/// Charge la configuration en suivant cet ordre (idempotent) :
/// 1) valeurs par défaut,
/// 2) fichier pointé par $PMS_CONFIG si présent,
/// 3) fallbacks: ./config.prod.toml, ./config/config.prod.toml, /etc/pms/config.prod.toml,
/// 4) variables d’env. préfixées PMS__ (double underscore pour sous-clés).
///
/// Exemple ENV: PMS__NETWORK__MODE, PMS__TLS__CERT_PEM, etc.
pub fn load_config() -> Result<Settings, ConfigError> {
    use config::{Config, File};
    use std::{env, path::PathBuf};

    let mut b = Config::builder();

    if let Ok(path) = env::var("PMS_CONFIG") {
        let pb = PathBuf::from(&path);
        b = b.add_source(File::from(pb).required(true));
    } else {
        // 1) Répertoire "config" classique (ce que tu avais déjà)
        let config_dir = PathBuf::from(ROOT_CONFIG_DIR);

        // 2) Répertoire "etc/config" à la racine du repo
        //    On remonte à la racine du repo à partir de crates/pms-config
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let etc_config_dir = repo_root.join("etc/config");

        let candidates = [
            // ancien chemin (si tu mets un jour dag-pms/config/config.dev.toml)
            config_dir.join("config.dev.toml"),
            // NOUVEAU: ton vrai dossier actuel
            etc_config_dir.join("config.dev.toml"),
            // chemins génériques
            PathBuf::from("config.dev.toml"),
            PathBuf::from("/etc/pms/config.dev.toml"),
        ];

        for pb in candidates {
            b = b.add_source(File::from(pb).required(false));
        }
    }

    let cfg = b.add_source(Environment::with_prefix("PMS").separator("__"));

    let mut settings: Settings = cfg.build()?.try_deserialize()?;

    if settings.network.network_id.is_empty() {
        settings.network.network_id = "pms-dev".into();
    }

    // NOTE: if-let combiné pour satisfaire clippy::collapsible_if
    if settings.network.mode.is_prod()
        && let Some(c) = settings.client.as_mut()
    {
        c.allow_insecure_tls = false;
    }

    settings.validate().expect("Settings invalide");
    Ok(settings)
}
pub fn load_config_with(arg: Option<&std::path::Path>) -> Result<Settings, LoadError> {
    let mut b = Config::builder()
        .set_default("rocks.path", "./data/pms-rocks")?
        .set_default("rocks.prefix", "pms:dev")?
        .set_default("network.mode", "dev")?
        .set_default("address.hrp", "8e")?
        // lb: admin.wallet_addresses is obsolete
        .set_default("client.oracle_url", "https://127.0.0.1:8080")?
        .set_default("client.allow_insecure_tls", true)?
        .set_default("tip_limit", 200)?
        .set_default("p2p.known_peers", String::new())?
        .add_source(Environment::with_prefix("PMS").separator("__"));

    if let Some(path) = arg {
        // Si on a fourni un fichier --config, il prime sur tout le reste
        b = b.add_source(File::from(path).required(true));
    } else if let Ok(env_path) = std::env::var("PMS_CONFIG") {
        // sinon, on respecte la variable d’environnement si elle existe
        let pb = PathBuf::from(&env_path);
        b = if pb.extension().is_some() || pb.is_absolute() {
            b.add_source(File::from(pb).required(false))
        } else {
            b.add_source(File::with_name(&env_path).required(false))
        };
    } else {
        // fallback standard
        let candidates = [
            "/config.dev.toml",
            "../config.dev.toml",
            "config/config.dev.toml",
            "../config/config.dev.toml",
            "/etc/pms/config.dev.toml",
        ];

        for p in candidates {
            b = b.add_source(File::from(PathBuf::from(p)).required(false));
        }
    }

    let mut s: Settings = b.build()?.try_deserialize()?;

    // garde-fou production
    // NOTE: if-let combiné pour satisfaire clippy::collapsible_if
    if s.network.mode.is_prod()
        && let Some(c) = s.client.as_mut()
    {
        c.allow_insecure_tls = false;
    }

    Ok(s)
}
