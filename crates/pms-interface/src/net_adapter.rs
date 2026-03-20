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

    /// Retourne le supply total en circulation (PMS natif) et le nombre d'UTXOs.
    async fn circulating_supply(&self) -> (rust_decimal::Decimal, u64);

    /// Retourne le supply en circulation d'un asset spécifique (None = PMS natif).
    async fn circulating_supply_by_asset(
        &self,
        asset_id: Option<&str>,
    ) -> (rust_decimal::Decimal, u64);

    /// Retourne la balance d'une adresse (somme des UTXOs non dépensés PMS).
    async fn balance_by_address(&self, address: &str) -> rust_decimal::Decimal;

    /// Retourne la balance d'une adresse pour un asset spécifique.
    /// `asset_id = None` → PMS natif (O(1) via cache).
    /// `asset_id = Some("edenite")` → balance custom token (shard scan).
    async fn balance_by_address_and_asset(
        &self,
        address: &str,
        asset_id: Option<&str>,
    ) -> rust_decimal::Decimal;

    /// Retourne tous les UTXOs d'une adresse depuis le set UTXO en mémoire.
    async fn utxos_by_address(
        &self,
        address: &str,
    ) -> Vec<(pms_types::OutputId, pms_types::TxOutput)>;

    /// Returns up to `limit` UTXOs for coin selection, stopping when enough
    /// value is accumulated. Avoids cloning ALL UTXOs for large addresses
    /// (e.g., coordinator with millions of fee reward UTXOs).
    ///
    /// Default implementation falls back to full `utxos_by_address()` + filter.
    /// `CoreAdapter` overrides with an optimized early-exit implementation.
    async fn utxos_for_selection(
        &self,
        address: &str,
        asset_id: &Option<String>,
        target: rust_decimal::Decimal,
        limit: usize,
    ) -> (
        Vec<(pms_types::OutputId, pms_types::TxOutput, rust_decimal::Decimal)>,
        rust_decimal::Decimal,
    ) {
        let all = self.utxos_by_address(address).await;
        let mut result = Vec::new();
        let mut total = rust_decimal::Decimal::ZERO;
        for (oid, txo) in all {
            if txo.asset_id != *asset_id {
                continue;
            }
            if let Ok(amt) = rust_decimal::Decimal::from_str_exact(&txo.amount) {
                result.push((oid, txo, amt));
                total += amt;
                if result.len() >= limit && total >= target {
                    break;
                }
            }
        }
        (result, total)
    }

    /// Ajoute un UTXO manuellement (utilisé par le coordinateur pour les EncryptedReward)
    async fn add_utxo(
        &self,
        txid: String,
        index: u32,
        address: String,
        amount: String,
        asset_id: Option<String>,
    );

    /// Supprime un UTXO du cache (utilisé quand un input est consommé par une TX encrypted)
    /// Retourne true si l'UTXO existait et a été supprimé, false sinon.
    async fn remove_utxo(&self, output_id: &pms_types::OutputId) -> bool;

    /// Récupère un UTXO spécifique par son OutputId depuis le set UTXO en mémoire.
    /// Retourne None si l'UTXO n'existe pas (déjà dépensé ou inexistant).
    async fn get_utxo(&self, output_id: &pms_types::OutputId) -> Option<pms_types::TxOutput>;

    /// Retourne l'EventBus pour s'abonner aux événements (SSE streaming).
    /// Default: None (mocks de test n'ont pas besoin d'event bus).
    fn event_bus(&self) -> Option<pms_event::EventBus> {
        None
    }
}
