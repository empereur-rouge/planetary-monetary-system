use async_trait::async_trait;
use anyhow::Result;

use crate::StoredBlock;

#[async_trait]
pub trait DagStorage: Send + Sync {
    async fn put_block(&self, b: &StoredBlock) -> Result<()>;
    async fn get_block(&self, id: &str) -> Result<Option<StoredBlock>>;

    async fn add_child_edge(&self, parent: &str, child: &str) -> Result<()>;
    async fn children_count(&self, id: &str) -> Result<u64>;

    async fn add_tip(&self, id: &str) -> Result<()>;
    async fn remove_tip(&self, id: &str) -> Result<()>;
    async fn top_tips(&self, limit: usize) -> Result<Vec<String>>;

    async fn all_block_ids(&self) -> Result<Vec<String>>;

    async fn export_json(&self) -> Result<String>;
    async fn import_json(&self, dump: &str) -> Result<()>;
}
