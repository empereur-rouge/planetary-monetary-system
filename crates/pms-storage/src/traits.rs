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

    /// Returns the total number of blocks in the DAG
    async fn block_count(&self) -> Result<u64>;

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
}
