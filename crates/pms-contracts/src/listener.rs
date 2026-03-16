//! Contract Listener — subscriber EventBus pour l'évaluation des contrats.
//!
//! Écoute les événements `NftBurnProcessed` sur l'EventBus et évalue
//! les contrats déclaratifs matchants. Les refunds sont poussés via le
//! trait [`RefundSink`] implémenté côté `pms-server`.
//!
//! ## Architecture
//!
//! Ce module ne dépend PAS de `pms-server` ni d'`AppState`. Il prend
//! ses dépendances de manière explicite :
//! - `EventBus` pour écouter les événements
//! - `Arc<dyn ContractStorage>` pour chercher les contrats
//! - `Arc<dyn RefundSink>` pour pousser les refunds

use pms_event::{EventBus, PmsEvent};
use pms_storage::ContractStorage;
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Trait d'abstraction pour l'accumulation des refunds de burn.
///
/// Implémenté côté `pms-server` par `FeePoolRefundSink` qui wrappe
/// le `FeePoolRegistry`. Permet à `pms-contracts` de ne pas dépendre
/// de `pms-server`.
pub trait RefundSink: Send + Sync {
    /// Accumule un refund de burn pour un wallet sur un ledger donné.
    ///
    /// # Arguments
    /// * `ledger_id` - ID du ledger cible (pour le bon FeePool)
    /// * `address` - Adresse bech32 du bénéficiaire
    /// * `amount` - Montant du refund
    /// * `asset_id` - `None` = PMS natif, `Some("edenite")` = custom token
    fn add_burn_refund(
        &self,
        ledger_id: &str,
        address: &str,
        amount: Decimal,
        asset_id: Option<String>,
    );
}

/// Lance le listener de contrats sur l'EventBus.
///
/// Spawne une tâche Tokio qui écoute les `NftBurnProcessed` et évalue
/// les contrats matchants. Les refunds sont poussés via `sink`.
///
/// # Arguments
/// * `bus` - EventBus pour s'abonner aux événements
/// * `contract_store` - Store de contrats (toujours le main RocksDB)
/// * `sink` - Implémentation RefundSink pour accumuler les refunds
pub fn spawn_contract_listener(
    bus: EventBus,
    contract_store: Arc<dyn ContractStorage>,
    sink: Arc<dyn RefundSink>,
) {
    let mut rx = bus.subscribe();

    tokio::spawn(async move {
        tracing::info!("ContractListener: started, listening for NftBurnProcessed events");

        loop {
            match rx.recv().await {
                Ok(PmsEvent::NftBurnProcessed {
                    block_id,
                    ledger_id,
                    burner_address,
                    token_ids,
                    metadata,
                }) => {
                    let nft_type = metadata.as_ref().and_then(|m| m.nft_type.as_deref());
                    let token_count = token_ids.len() as u64;

                    let results = crate::engine::evaluate_nft_burn(
                        contract_store.as_ref(),
                        &ledger_id,
                        &burner_address,
                        nft_type,
                        metadata.as_ref(),
                        token_count,
                    );

                    for r in &results {
                        sink.add_burn_refund(
                            &ledger_id,
                            &r.refund_address,
                            r.refund_amount,
                            r.asset_id.clone(),
                        );
                        tracing::info!(
                            "ContractListener: '{}' burn refund {} {} for {} on ledger={} (block={})",
                            r.contract_name,
                            r.refund_amount,
                            r.asset_id.as_deref().unwrap_or("PMS"),
                            &r.refund_address[..20.min(r.refund_address.len())],
                            ledger_id,
                            &block_id[..16.min(block_id.len())]
                        );
                    }

                    if !results.is_empty() {
                        // Emit ContractFulfilled events for each result
                        for r in &results {
                            bus.emit(PmsEvent::ContractFulfilled {
                                block_id: block_id.clone(),
                                contract_id: r.contract_id.clone(),
                                result: r.details.clone(),
                            });
                        }
                    }
                }

                // Ignore all other event types
                Ok(_) => {}

                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        "ContractListener: lagged, dropped {} events — consider increasing EventBus capacity",
                        n
                    );
                }

                Err(broadcast::error::RecvError::Closed) => {
                    tracing::info!("ContractListener: EventBus closed, shutting down");
                    break;
                }
            }
        }
    });
}
