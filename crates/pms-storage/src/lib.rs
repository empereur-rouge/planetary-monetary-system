// NOTE: Ces allow sont temporaires pour le pré-déploiement.
// Les collapsible_if dans store.rs sont corrects mais verbeux.
// Le type_complexity est un faux positif pour un itérateur simple.
#![allow(clippy::collapsible_if)]
#![allow(clippy::type_complexity)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::useless_conversion)]
extern crate core;

pub mod activity_item;
mod checkpoint_rocks;
pub mod compliance_store;
pub mod config_store;
pub mod contract_store;
pub mod coordinator_key_store;
pub mod gas_pool_store;
pub mod ledger_store;
pub mod helpers;
pub mod migrations;
pub mod models;
mod mutation;
pub mod nft_store;
pub mod node_rewards;
pub mod rocks_store;
pub mod store;
pub mod traits;

pub use activity_item::*;
pub use compliance_store::*;
pub use config_store::*;
pub use contract_store::*;
pub use coordinator_key_store::*;
pub use gas_pool_store::*;
pub use ledger_store::*;
pub use migrations::*;
pub use models::*;
pub use nft_store::*;
pub use node_rewards::*;
pub use store::*;
pub use traits::*;
pub use utxo::*;

pub use checkpoint_rocks::*;
pub use mutation::*;
pub use rocks_store::*;

// ═══════════════════════════════════════════════════════════════════════
// DAG SemVer — version du protocole DAG
// ═══════════════════════════════════════════════════════════════════════

/// Version SemVer parsée (major.minor.patch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagSemVer {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl DagSemVer {
    /// Parse une string SemVer "X.Y.Z".
    pub fn parse(version: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = version.split('.').collect();
        if parts.len() != 3 {
            anyhow::bail!("Invalid SemVer format: '{}' (expected X.Y.Z)", version);
        }
        Ok(Self {
            major: parts[0]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid major version in '{}'", version))?,
            minor: parts[1]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid minor version in '{}'", version))?,
            patch: parts[2]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid patch version in '{}'", version))?,
        })
    }
}

impl std::fmt::Display for DagSemVer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl PartialOrd for DagSemVer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DagSemVer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
    }
}

/// Résultat de la vérification de compatibilité DAG.
#[derive(Debug, PartialEq, Eq)]
pub enum VersionCheck {
    /// Même major, stored <= current → OK (migration auto si besoin)
    Compatible,
    /// Major mismatch (stored.major < current.major) → breaking change
    MajorMismatch,
    /// Stored > current → données créées par une version plus récente
    Downgrade,
}

/// Compare la version stockée dans le DAG avec la version courante du binaire.
pub fn check_dag_compatibility(stored: &DagSemVer, current: &DagSemVer) -> VersionCheck {
    // Downgrade : la DB a été utilisée par une version plus récente
    if stored > current {
        return VersionCheck::Downgrade;
    }
    // Major mismatch : breaking change
    if stored.major != current.major {
        return VersionCheck::MajorMismatch;
    }
    // Même major, stored <= current → compatible (auto-migrate minor/patch)
    VersionCheck::Compatible
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn test_parse_semver() {
        let v = DagSemVer::parse("1.2.3").unwrap();
        println!("Parsed SemVer: {v:?} -> Display: {v}");
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert_eq!(v.to_string(), "1.2.3");
    }

    #[test]
    fn test_parse_semver_invalid() {
        let bad = DagSemVer::parse("1.2");
        println!("Parse '1.2': {bad:?}");
        assert!(bad.is_err());

        let bad2 = DagSemVer::parse("abc");
        println!("Parse 'abc': {bad2:?}");
        assert!(bad2.is_err());

        let bad3 = DagSemVer::parse("1.2.x");
        println!("Parse '1.2.x': {bad3:?}");
        assert!(bad3.is_err());
    }

    #[test]
    fn test_compat_same_version() {
        let v1 = DagSemVer::parse("1.0.0").unwrap();
        let v2 = DagSemVer::parse("1.0.0").unwrap();
        let result = check_dag_compatibility(&v1, &v2);
        println!("1.0.0 vs 1.0.0 = {result:?}");
        assert_eq!(result, VersionCheck::Compatible);
    }

    #[test]
    fn test_compat_minor_upgrade() {
        let stored = DagSemVer::parse("1.0.0").unwrap();
        let current = DagSemVer::parse("1.1.0").unwrap();
        let result = check_dag_compatibility(&stored, &current);
        println!("stored=1.0.0 vs current=1.1.0 = {result:?}");
        assert_eq!(result, VersionCheck::Compatible);
    }

    #[test]
    fn test_compat_patch_upgrade() {
        let stored = DagSemVer::parse("1.0.0").unwrap();
        let current = DagSemVer::parse("1.0.1").unwrap();
        let result = check_dag_compatibility(&stored, &current);
        println!("stored=1.0.0 vs current=1.0.1 = {result:?}");
        assert_eq!(result, VersionCheck::Compatible);
    }

    #[test]
    fn test_compat_major_breaking() {
        let stored = DagSemVer::parse("1.5.0").unwrap();
        let current = DagSemVer::parse("2.0.0").unwrap();
        let result = check_dag_compatibility(&stored, &current);
        println!("stored=1.5.0 vs current=2.0.0 = {result:?}");
        assert_eq!(result, VersionCheck::MajorMismatch);
    }

    #[test]
    fn test_compat_downgrade_minor() {
        let stored = DagSemVer::parse("1.1.0").unwrap();
        let current = DagSemVer::parse("1.0.0").unwrap();
        let result = check_dag_compatibility(&stored, &current);
        println!("stored=1.1.0 vs current=1.0.0 = {result:?}");
        assert_eq!(result, VersionCheck::Downgrade);
    }

    #[test]
    fn test_compat_downgrade_major() {
        let stored = DagSemVer::parse("2.0.0").unwrap();
        let current = DagSemVer::parse("1.5.0").unwrap();
        let result = check_dag_compatibility(&stored, &current);
        println!("stored=2.0.0 vs current=1.5.0 = {result:?}");
        assert_eq!(result, VersionCheck::Downgrade);
    }
}
