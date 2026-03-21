use crate::{PutResult, StoredBlock, UtxoDelta};
use anyhow::Result;
use async_trait::async_trait;
use pms_wire::WireBlock;

#[async_trait]
pub trait DagStorage: Send + Sync {
    async fn put_block(&self, b: &StoredBlock) -> Result<PutResult>;
    async fn get_block(&self, id: &str) -> Result<Option<StoredBlock>>;

    async fn add_child_edge(&self, parent: &str, child: &str) -> Result<()>;
    async fn children_count(&self, id: &str) -> Result<u64>;

    async fn add_tip(&self, id: &str) -> Result<()>;
    async fn remove_tip(&self, id: &str) -> Result<()>;
    async fn top_tips(&self, limit: usize) -> Result<Vec<String>>;

    async fn all_block_ids(&self) -> Result<Vec<String>>;

    /// Returns the `n` lexicographically largest block IDs (i.e. the "newest"
    /// in RocksDB key order).  When `n == 0`, returns **all** IDs (same as
    /// `all_block_ids()`).
    ///
    /// The default implementation falls back to `all_block_ids()` + truncation.
    /// `RocksStore` overrides this with a **reverse iterator** so that only `n`
    /// keys are ever read — critical for ledgers with millions of blocks where
    /// loading all IDs would consume hundreds of MB of RAM.
    async fn newest_block_ids(&self, n: usize) -> Result<Vec<String>> {
        let mut all = self.all_block_ids().await?;
        if n > 0 && all.len() > n {
            all = all.split_off(all.len() - n);
        }
        Ok(all)
    }

    /// O(1) check whether the storage contains any blocks.
    ///
    /// Default implementation falls back to `all_block_ids()`.
    /// `RocksStore` overrides with a single iterator seek on `idx_blocks` CF.
    async fn is_empty(&self) -> Result<bool> {
        Ok(self.all_block_ids().await?.is_empty())
    }

    /// Returns up to `n` block IDs ordered by insertion timestamp (newest first
    /// in the iterator, reversed to oldest-first in the returned Vec).
    ///
    /// Uses the `by_time` index for true chronological ordering.  This is
    /// critical for bootstrap: consecutive blocks reference recent parents,
    /// so loading the N most-recent blocks preserves parent-child locality
    /// and minimises orphan tips (~2-5% vs ~99.8% with lexicographic order).
    ///
    /// Falls back to `newest_block_ids(n)` (lexicographic) when the time
    /// index is unavailable (e.g. `MockStore`, or blocks imported via
    /// `import_json` which skips the `by_time` CF).
    async fn newest_block_ids_by_time(&self, n: usize) -> Result<Vec<String>> {
        self.newest_block_ids(n).await
    }

    /// Returns the total number of blocks in the DAG
    async fn block_count(&self) -> Result<u64>;

    /// Returns an **approximate** block count using storage metadata.
    ///
    /// `RocksStore` uses RocksDB's `estimate-num-keys` property (O(1)).
    /// Default falls back to the exact (but potentially slow) `block_count()`.
    async fn block_count_estimate(&self) -> Result<u64> {
        self.block_count().await
    }

    async fn export_json(&self) -> Result<String>;
    async fn export_namespace(&self) -> Result<String>;
    async fn import_json(&self, dump: &str) -> Result<()>;

    async fn append_block_atomic(&self, b: &StoredBlock) -> Result<bool>;
    async fn load_final(&self) -> Result<Vec<String>>;
    async fn load_last_milestone(&self) -> Result<Option<String>>;

    /// Derniers ids insérés (du plus récent au plus ancien), borne par `limit`.
    async fn recent_ids(&self, limit: usize) -> Result<Vec<String>>;
    /// Itère les IDs du plus récent au plus ancien, avec curseur (ts,id).
    /// `after_ts`/`after_id` = point de reprise exclusif (on retourne Strictement plus anciens).
    /// Retourne: (ids, next_cursor=(ts,id,has_more))
    async fn recent_ids_by_time(
        &self,
        after_ts: Option<i64>,
        after_id: Option<String>,
        limit: usize,
    ) -> Result<(Vec<String>, Option<(i64, String, bool)>)>;

    /// Récupère un lot de blocs complets (ordre identique à `ids' ; ceux manquants sont ignorés).
    async fn get_blocks_by_ids(&self, ids: &[String]) -> Result<Vec<WireBlock>>;

    async fn persist_final(&self, ids: &[String]) -> Result<()>;
    async fn persist_last_milestone(&self, id: &str) -> Result<()>;
    async fn append_block_atomic_with_utxo(
        &self,
        b: &StoredBlock,
        delta: Option<&UtxoDelta>,
    ) -> Result<bool>;

    /// Persist multiple blocks in a single atomic write.
    ///
    /// Returns the number of NEW blocks persisted (duplicates are skipped).
    /// Default implementation falls back to calling `append_block_atomic_with_utxo`
    /// for each block sequentially. RocksStore overrides with a single `WriteBatch`
    /// for all blocks — reducing WAL appends and mutex acquisitions by up to 64×.
    async fn append_blocks_batch(
        &self,
        blocks: &[(&StoredBlock, Option<&UtxoDelta>)],
    ) -> Result<usize> {
        let mut count = 0usize;
        for (b, delta) in blocks {
            if self.append_block_atomic_with_utxo(b, *delta).await? {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Paginated reverse-chronological scan of block IDs involving a specific
    /// address.  Returns `(block_ids, next_cursor)`.
    /// Default no-op returns empty results (used by non-RocksDB backends).
    async fn recent_ids_by_address(
        &self,
        _addr: &str,
        _after_ts: Option<i64>,
        _after_id: Option<String>,
        _limit: usize,
    ) -> Result<(Vec<String>, Option<(i64, String, bool)>)> {
        Ok((vec![], None))
    }

    /// Like `recent_ids_by_address` but filtered by one or more activity
    /// categories (see `ActivityCategory`).  When a single category is given
    /// it's a simple prefix scan; multiple categories trigger a k-way merge.
    /// Default no-op returns empty results (used by non-RocksDB backends).
    async fn recent_ids_by_address_and_categories(
        &self,
        _addr: &str,
        _categories: &[u8],
        _after_ts: Option<i64>,
        _after_id: Option<String>,
        _limit: usize,
    ) -> Result<(Vec<String>, Option<(i64, String, bool)>)> {
        Ok((vec![], None))
    }
}
