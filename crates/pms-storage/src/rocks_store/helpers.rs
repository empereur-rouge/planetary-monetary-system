use crate::rocks_store::store::RocksStore;
use anyhow::{Context, Result};
use chrono::Utc;
use rocksdb::checkpoint::Checkpoint;
use std::path::PathBuf;

impl RocksStore {
    /// Flush le WAL sur disque.
    /// Côté RocksDB: db.flush_wal(true) = flush WAL et fsync.
    pub async fn flush_wal(&self) -> Result<()> {
        self.db.flush_wal(true).context("flush_wal(true)")?;
        Ok(())
    }

    /// Compacte toutes les column families connues.
    /// On boucle sur la liste des CF qu’on utilise dans new().
    pub async fn compact_all(&self) -> Result<()> {
        // Même liste logique que dans new() / ensure_schema()
        const CF_NAMES: &[&str] = &[
            "default",
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
            "tx_applied",
        ];

        for name in CF_NAMES {
            if let Some(cf) = self.db.cf_handle(name) {
                // None/None = compacter toute la plage de la CF
                self.db
                    .compact_range_cf::<&[u8], &[u8]>(&cf, None::<&[u8]>, None::<&[u8]>);
            }
        }

        eprintln!("[rocks] compact_all: compaction triggered on all CFs");
        Ok(())
    }

    /// Log de quelques propriétés RocksDB pour avoir de la visibilité.
    pub async fn log_stats(&self) -> Result<()> {
        if let Ok(Some(stats)) = self.db.property_value("rocksdb.stats") {
            eprintln!("[rocks][stats]\n{}", stats);
        }
        if let Ok(Some(ldb)) = self.db.property_value("rocksdb.levelstats") {
            eprintln!("[rocks][levelstats]\n{}", ldb);
        }
        Ok(())
    }

    /// Crée un snapshot RocksDB dans `backup_root`.
    ///
    /// - `backup_root` est le dossier racine des backups (ex: `/var/backups/pms`).
    /// - La fonction crée au besoin `backup_root`.
    /// - Le checkpoint lui-même sera un sous-dossier du type `rocks-YYYYMMDD-HHMMSS`.
    ///
    /// ⚠ Très important : RocksDB exige que le dossier cible du checkpoint
    ///    **n'existe pas** avant l'appel à `create_checkpoint`.
    pub fn create_checkpoint(&self, backup_root: &str) -> Result<()> {
        // 1) S'assurer que le dossier racine des backups existe
        let root = PathBuf::from(backup_root);
        std::fs::create_dir_all(&root)
            .with_context(|| format!("create_dir_all(backup_root={})", root.display()))?;

        // 2) Construire un sous-dossier unique avec timestamp
        //    Exemple: /backups/rocks-20251203-101530
        let ts = Utc::now().format("%Y%m%d-%H%M%S").to_string();
        let dest = root.join(format!("rocks-{ts}"));

        // ⚠ Ne PAS créer `dest` à l'avance :
        //    - create_checkpoint() attend un dossier INEXISTANT.
        let cp = Checkpoint::new(&*self.db).context("Checkpoint::new")?;

        cp.create_checkpoint(&dest)
            .with_context(|| format!("create_checkpoint({})", dest.display()))?;

        eprintln!("[rocks] checkpoint created at {}", dest.display());
        Ok(())
    }
}
