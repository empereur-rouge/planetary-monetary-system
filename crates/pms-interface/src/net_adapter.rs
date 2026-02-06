use anyhow::Result;
use async_trait::async_trait;
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
    /// [DEPRECATED] PoW is disabled for Private DAG. Returns 0.
    /// Kept for API compatibility, will be removed in a future version.
    fn min_pow_leading_zero_bits(&self) -> u8;

    /// Retourne le supply total en circulation et le nombre d'UTXOs.
    async fn circulating_supply(&self) -> (rust_decimal::Decimal, u64);

    /// Retourne la balance d'une adresse (somme des UTXOs non dépensés).
    async fn balance_by_address(&self, address: &str) -> rust_decimal::Decimal;

    /// Retourne tous les UTXOs d'une adresse depuis le set UTXO en mémoire.
    async fn utxos_by_address(&self, address: &str) -> Vec<(pms_types::OutputId, pms_types::TxOutput)>;

    /// Ajoute un UTXO manuellement (utilisé par le coordinateur pour les EncryptedReward)
    async fn add_utxo(&self, txid: String, index: u32, address: String, amount: String);
}
