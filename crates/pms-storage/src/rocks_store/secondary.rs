//! Read-only and secondary RocksDB instances.
//!
//! Used for analytics, monitoring, and backup scenarios where the main DB
//! should not be locked exclusively.

use crate::rocks_store::store::{PmsDb, RocksStore};
use anyhow::{Context, Result};
use rocksdb::Options;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::time::Duration;

impl RocksStore {
    pub async fn open_read_only(
        path: &str,
        tip_limit: usize,
        _prefix: &str,
    ) -> anyhow::Result<Self> {
        let mut opts = Options::default();
        opts.create_if_missing(false);

        // false => pas d'erreur si des WAL existent, pas de lock exclusif
        let db = PmsDb::open_for_read_only(&opts, path, false)
            .map_err(|e| anyhow::anyhow!("open_read_only: {e}"))?;

        let prefix = "".to_string();
        let cf_names = Self::build_cf_names(&prefix);
        let db_path = PathBuf::from(path);
        Ok(Self {
            db: Arc::new(db),
            tip_limit,
            prefix,
            checkpoint_interval: Duration::from_secs(24 * 3600),
            db_path,
            tip_count_estimate: std::sync::atomic::AtomicUsize::new(0),
            top_tips_cache: parking_lot::Mutex::new(None),
            cf_names,
            runtime_config_cache: parking_lot::Mutex::new(None),
            frozen_set: dashmap::DashSet::new(),
            persist_counter: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn refresh_from_primary(&self) -> anyhow::Result<()> {
        self.db
            .try_catch_up_with_primary()
            .map_err(|e| anyhow::anyhow!("catch_up: {e}"))
    }

    pub async fn open_secondary(
        primary_path: &str,
        secondary_path: &str,
        tip_limit: usize,
        prefix: impl Into<String>,
    ) -> Result<Self> {
        let prefix = prefix.into();

        let primary = PathBuf::from(primary_path);
        let secondary = PathBuf::from(secondary_path);

        std::fs::create_dir_all(&secondary)
            .with_context(|| format!("create_dir_all({})", secondary.display()))?;

        let mut db_opts = Options::default();
        db_opts.create_if_missing(false);
        db_opts.create_missing_column_families(false);

        let required: BTreeSet<String> = [
            "blocks",
            "idx_blocks",
            "by_time",
            "id2ts",
            "final",
            "last_ms",
            "children_count",
            "tips",
            "children_set",
            "ver",
            "utxo",
            "utxo_spent",
            "tx_applied",
            "nft_ownership",
            "nfts_by_owner",
            "nft_block_ids",
            "compliance_frozen",
            "compliance_log",
        ]
        .into_iter()
        .map(|s| format!("{prefix}:{s}"))
        .collect();

        // Juste les noms de CF
        let mut cf_names: Vec<String> = Vec::with_capacity(required.len() + 1);
        cf_names.push("default".to_string());
        cf_names.extend(required.into_iter());

        let db = PmsDb::open_cf_as_secondary(&db_opts, &primary, &secondary, &cf_names)
            .with_context(|| {
                format!(
                    "open RocksDB secondary at {} (primary={})",
                    secondary.display(),
                    primary.display()
                )
            })?;

        let cf_names = Self::build_cf_names(&prefix);
        Ok(Self {
            db: Arc::new(db),
            tip_limit,
            prefix,
            checkpoint_interval: Duration::from_secs(24 * 3600),
            db_path: primary,
            tip_count_estimate: std::sync::atomic::AtomicUsize::new(0),
            top_tips_cache: parking_lot::Mutex::new(None),
            cf_names,
            runtime_config_cache: parking_lot::Mutex::new(None),
            frozen_set: dashmap::DashSet::new(),
            persist_counter: std::sync::atomic::AtomicU64::new(0),
        })
    }
}
