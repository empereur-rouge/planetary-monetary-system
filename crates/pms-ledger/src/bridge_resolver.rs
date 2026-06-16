//! Cross-ledger `BridgeMint` reconciliation resolver (audit rang 3, B3).
//!
//! A `BridgeMint` on a destination ledger must back a real `BridgeLock` on its
//! source ledger (same amount/asset/recipient). The destination `CoreAdapter`
//! only sees its own prefix-scoped store, so its persist path delegates the
//! source-lock lookup to this resolver, which holds a shared `ledger_id -> source`
//! map covering every ledger.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use pms_core::ConcurrentDag;
use pms_interface::{BridgeLockInfo, BridgeLockResolver};
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_types::{PayloadEnvelope, PlainPayload};

/// In-RAM DAG + durable store of a source ledger, used to resolve a `BridgeLock`.
///
/// The bridge engine persists the source `BridgeLock` then the destination
/// `BridgeMint` back-to-back; the lock's durable RocksDB write is asynchronous
/// (background persist queue), so at mint-reconciliation time the lock is in the
/// source DAG's RAM but may not yet be on disk. We therefore read RAM FIRST and
/// fall back to the store (which covers a post-restart re-validation, when the
/// lock survives only on disk).
#[derive(Clone)]
pub struct LedgerLockSource {
    /// Source ledger's lock-free DAG (RAM, immediately consistent).
    pub dag: Arc<ConcurrentDag>,
    /// Source ledger's durable store (post-restart fallback).
    pub store: Arc<RocksStore>,
}

/// Resolves source-ledger `BridgeLock`s from a shared `ledger_id -> source` map.
///
/// Holds only DAGs + stores (never `LedgerInstance`/adapters), so there is **no
/// `Arc` cycle** with the adapters that in turn hold this resolver. The map is
/// shared (same `Arc`) with the [`crate::LedgerManager`], so a ledger added at
/// runtime (`add_ledger`) becomes resolvable automatically.
pub struct LedgerStoreResolver {
    sources: Arc<DashMap<String, LedgerLockSource>>,
}

impl LedgerStoreResolver {
    /// Build a resolver over the shared `ledger_id -> source` map.
    pub fn new(sources: Arc<DashMap<String, LedgerLockSource>>) -> Self {
        Self { sources }
    }
}

/// Extract `BridgeLockInfo` from a payload, if it is a `BridgeLock`.
fn lock_info_from_payload(payload: &PayloadEnvelope) -> Option<BridgeLockInfo> {
    match payload {
        PayloadEnvelope::Plain(PlainPayload::BridgeLock {
            amount,
            asset_id,
            dest_ledger_id,
            dest_address,
            ..
        }) => Some(BridgeLockInfo {
            amount: amount.clone(),
            asset_id: asset_id.clone(),
            dest_ledger_id: dest_ledger_id.clone(),
            dest_address: dest_address.clone(),
        }),
        _ => None,
    }
}

#[async_trait]
impl BridgeLockResolver for LedgerStoreResolver {
    async fn resolve_bridge_lock(
        &self,
        source_ledger_id: &str,
        lock_block_id: &str,
    ) -> Result<Option<BridgeLockInfo>> {
        // Unknown source ledger → no lock (the caller fails closed).
        let Some(source) = self.sources.get(source_ledger_id).map(|s| s.clone()) else {
            return Ok(None);
        };

        // 1) RAM DAG first — the lock is present here the moment the source
        //    `persist_block` returns, even before its async durable write lands.
        if let Some(block) = source.dag.get_block(lock_block_id) {
            return Ok(block.payload.as_ref().and_then(lock_info_from_payload));
        }

        // 2) Durable store fallback — covers a post-restart re-validation where
        //    the lock survives only on disk (RAM DAG rebuilt without it, or
        //    pruned). A block that exists but is not a BridgeLock → not a lock.
        let Some(sb) = source.store.get_block(lock_block_id).await? else {
            return Ok(None);
        };
        let Some(pjson) = sb.payload_json else {
            return Ok(None);
        };
        match serde_json::from_str::<PayloadEnvelope>(&pjson) {
            Ok(payload) => Ok(lock_info_from_payload(&payload)),
            Err(_) => Ok(None),
        }
    }
}
