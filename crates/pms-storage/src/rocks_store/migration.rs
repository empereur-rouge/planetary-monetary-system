use crate::rocks_store::store::RocksStore;
use crate::{CURRENT_VER, DagStorage, MigError};
use anyhow::anyhow;
use std::sync::Arc;
use rocksdb::BoundColumnFamily;

impl RocksStore {
    // --- helpers internes versionning ------------------------------------

    fn cf_ver(&self) -> Arc<BoundColumnFamily<'_>> {
        self.cf("ver")
    }

    pub async fn get_version(&self) -> anyhow::Result<i64> {
        if let Some(v) = self.db.get_cf(&self.cf_ver(), b"ver")? {
            let s = String::from_utf8(v)?;
            let parsed: i64 = s.parse().unwrap_or(0);
            Ok(parsed)
        } else {
            Ok(0)
        }
    }

    async fn set_version(&self, v: i64) -> anyhow::Result<()> {
        let s = v.to_string();
        self.db.put_cf(&self.cf_ver(), b"ver", s.as_bytes())?;
        Ok(())
    }

    // --- API publique équivalente à RedisStore::ensure_schema  -----------

    pub async fn ensure_schema(&self) -> Result<(), MigError> {
        // lire version courante (défaut = 0)
        let mut v = self.get_version().await.map_err(MigError::Any)?;

        while v < CURRENT_VER {
            match v {
                0 => self.mig_0_to_1().await?,
                1 => self.mig_1_to_2().await?,
                _ => return Err(MigError::Unexpected(v)),
            }
            v += 1;

            // si migration OK → on persiste la nouvelle version
            self.set_version(v).await.map_err(MigError::Any)?;
        }

        Ok(())
    }

    // --- migrations ------------------------------------------------------

    // Migration 0 -> 1 :
    // Redis faisait un SADD/SREM "__init__" pour forcer l'init de l'index des blocks.
    // Ici on simule la même chose en écrivant puis supprimant une clé factice
    // dans la CF idx_blocks. Idempotent et cheap.
    async fn mig_0_to_1(&self) -> std::result::Result<(), MigError> {
        let cf_idx = self.cf("idx_blocks");

        // Avant: .map_err(MigError::Any)?
        // Après: closure qui convertit rocksdb::Error -> anyhow::Error -> MigError
        self.db
            .put_cf(&cf_idx, b"__init__", b"")
            .map_err(|e| MigError::Any(anyhow!(e)))?;

        self.db
            .delete_cf(&cf_idx, b"__init__")
            .map_err(|e| MigError::Any(anyhow!(e)))?;

        Ok(())
    }

    // Migration 1 -> 2 :
    //
    // Objectif identique à Redis:
    //  - reconstruire l'état "tips" en considérant tous les blocs persistés
    //  - retirer les parents des tips
    //
    // Algo:
    //  for each block B:
    //     add_tip(B.id)
    //     for each parent P in B.parents:
    //         remove_tip(P)
    //
    // On NE reconstruit PAS ici children_count / children_set pour éviter
    // les doubles-incréments si la migration est rejouée (même choix que Redis).
    async fn mig_1_to_2(&self) -> std::result::Result<(), MigError> {
        let ids = self.all_block_ids().await.map_err(MigError::Any)?;
        let total = ids.len();
        tracing::info!("Migration 1→2: rebuilding tips for {} blocks", total);

        for (i, id) in ids.iter().enumerate() {
            if i > 0 && i % 10_000 == 0 {
                tracing::info!("Migration 1→2: processed {}/{} blocks ({:.1}%)", i, total, (i as f64 / total as f64) * 100.0);
            }
            let maybe_b = self.get_block(&id).await.map_err(MigError::Any)?;
            let b = match maybe_b {
                Some(b) => b,
                None => continue,
            };

            self.add_tip(&b.id).await.map_err(MigError::Any)?;

            for p in &b.parents {
                let _ = self.remove_tip(p).await;
            }
        }

        tracing::info!("Migration 1→2: completed ({} blocks processed)", total);
        Ok(())
    }
}
