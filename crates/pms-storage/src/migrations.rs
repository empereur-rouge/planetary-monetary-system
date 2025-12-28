use anyhow::Result;
use thiserror::Error;

use crate::{DagStorage, RedisStore};

pub const CURRENT_VER: i64 = 2;

#[derive(Error, Debug)]
pub enum MigError {
    #[error("unexpected version {0}")]
    Unexpected(i64),
    #[error(transparent)]
    Any(#[from] anyhow::Error),
}

impl RedisStore {
    pub async fn ensure_schema(&self) -> Result<(), MigError> {
        let mut con = self.con.clone();
        let ver: Option<i64> = redis::AsyncCommands::get(&mut con, Self::k_ver())
            .await
            .map_err(anyhow::Error::from)?;
        let mut v = ver.unwrap_or(0);

        while v < CURRENT_VER {
            match v {
                0 => self.mig_0_to_1().await?,
                1 => self.mig_1_to_2().await?,
                _ => return Err(MigError::Unexpected(v)),
            }
            v += 1;
            let _: () = redis::AsyncCommands::set(&mut con, Self::k_ver(), v)
                .await
                .map_err(anyhow::Error::from)?;
        }
        Ok(())
    }

    async fn mig_0_to_1(&self) -> Result<()> {
        let mut con = self.con.clone();
        let _: () = redis::AsyncCommands::sadd(&mut con, Self::k_idx_blocks(), "__init__").await?;
        let _: () = redis::AsyncCommands::srem(&mut con, Self::k_idx_blocks(), "__init__").await?;
        Ok(())
    }

    async fn mig_1_to_2(&self) -> Result<()> {
        let ids = self.all_block_ids().await?;
        for id in ids {
            if let Some(b) = self.get_block(&id).await? {
                self.add_tip(&b.id).await?;
                for p in &b.parents {
                    let _ = self.remove_tip(p).await;
                }
            }
        }
        Ok(())
    }
}
