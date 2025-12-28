use async_trait::async_trait;
use anyhow::Result;
use pms_storage::store::PutResult;
use pms_wire::WireBlock;


/// Trait que le serveur réseau utilisera pour interagir avec le core.
#[async_trait]
pub trait NetDagAdapter: Send + Sync {
    /// Vérifie si on possède déjà ce bloc.
    async fn have_block(&self, id: &str) -> bool;
    /// Persiste un bloc (idempotent).
    async fn persist_block(&self, b: &WireBlock) -> Result<PutResult>;
    /// Diffuse un bloc aux pairs.
    async fn broadcast_block(&self, b: &WireBlock) -> Result<()>;
    async fn top_tips(&self, limit: usize) -> Result<Vec<String>>;
    async fn get_block(&self, id: &str) -> Result<Option<WireBlock>>;
    async fn recent_ids(&self, limit: usize) -> Result<Vec<String>>;
    async fn get_blocks_by_ids(&self, ids: &[String]) -> Result<Vec<WireBlock>>;
}