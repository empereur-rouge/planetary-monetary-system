use async_trait::async_trait;
use anyhow::Result;
use pms_storage::{StoredBlock};

#[async_trait]
pub trait DagAdapter: Send + Sync + 'static {
    async fn have_block(&self, id: &str) -> bool;
    async fn persist_block(&self, b: &StoredBlock) -> Result<bool>;
    /// Diffuse un bloc sur le réseau **si** un serveur est attaché.
    /// - “Fire‑and‑forget” : si pas de serveur (ex: mode offline), on ne renvoie pas d’erreur.
    async fn broadcast_block(&self, b: &StoredBlock) -> Result<()>;
}