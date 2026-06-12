//! Block query helpers for the `NetDagAdapter` implementation.
//!
//! Provides `have_block`, `get_block`, `recent_ids`, `top_tips`,
//! `get_blocks_by_ids`, and `broadcast_block` as `pub(super)` methods
//! on `CoreAdapter` so the trait impl in `mod.rs` can delegate to them.

use crate::CoreAdapter;
use anyhow::Result;
use pms_storage::coordinator_key_store::CoordinatorKeyStorage;
use pms_storage::{ComplianceStorage, ConfigStorage, DagStorage, NftStorage, NodeRewardsStorage};
use pms_wire::WireBlock;

impl<S> CoreAdapter<S>
where
    S: DagStorage
        + NftStorage
        + ConfigStorage
        + NodeRewardsStorage
        + ComplianceStorage
        + CoordinatorKeyStorage
        + pms_storage::TokenRegistryStorage
        + Send
        + Sync
        + 'static,
{
    /// Est-ce que j'ai deja ce bloc en RAM ?
    ///
    /// - Sert a court-circuiter la reception reseau (evite doublons).
    pub(super) async fn do_have_block(&self, id: &str) -> bool {
        // rapide: regarde d'abord en RAM (lock-free)
        if self.dag.contains_block(id) {
            return true;
        }
        // fallback: store
        self.store.get_block(id).await.ok().flatten().is_some()
    }

    /// Retrieve a single block by id from the persistent store and convert
    /// it to a `WireBlock`.
    pub(super) async fn do_get_block(&self, id: &str) -> Result<Option<WireBlock>> {
        if let Some(sb) = self.store.get_block(id).await? {
            return Ok(Some(WireBlock {
                id: sb.id,
                parents: sb.parents,
                payload_json: sb.payload_json,
                nonce: sb.nonce,
                network_id: sb.network_id,
                protocol_version: sb.protocol_version,
                signer_pk_hex: sb.signer_pk_hex,
                signature_hex: sb.signature_hex,
                metadata: sb.metadata,
            }));
        }
        Ok(None)
    }

    /// Return the most recent block ids from the store.
    pub(super) async fn do_recent_ids(&self, limit: usize) -> Result<Vec<String>> {
        self.store.recent_ids(limit).await
    }

    /// Fetch multiple blocks by id from the store.
    pub(super) async fn do_get_blocks_by_ids(&self, ids: &[String]) -> Result<Vec<WireBlock>> {
        let sbs = self.store.get_blocks_by_ids(ids).await?;
        Ok(sbs)
    }

    /// Select the best tips for parent selection.
    ///
    /// In Single Writer mode, returns the single chain head from RAM DAG.
    /// Otherwise, queries the store first and falls back to RAM.
    pub(super) async fn do_top_tips(&self, limit: usize) -> Result<Vec<String>> {
        // [SINGLE WRITER] Optimisation : Selection lineaire simple
        if self.policy.enforce_single_writer {
            // FIX: Check RAM DAG first (contains most recent blocks)
            // This fixes race condition where blocks are in DAG but not yet in RocksDB
            let dag_tips = self.dag.find_tips();
            if !dag_tips.is_empty() {
                // Return latest tip from RAM (most up-to-date)
                // In linear chain mode, find_tips() returns 1 tip (the chain head)
                tracing::debug!(
                    count = dag_tips.len(),
                    last = ?dag_tips.last(),
                    "top_tips from RAM DAG"
                );
                return Ok(vec![dag_tips[dag_tips.len() - 1].clone()]);
            }

            // Fallback to store if DAG is empty (shouldn't happen after bootstrap)
            if let Ok(recents) = self.store.recent_ids(1).await {
                if !recents.is_empty() {
                    return Ok(recents);
                }
            }
        }

        // 1) essaye le store s'il l'expose
        if let Ok(v) = self.store.top_tips(limit).await {
            if !v.is_empty() {
                tracing::debug!(count = v.len(), "top_tips from store");
                return Ok(v);
            }
        }

        // 2) fallback RAM: DAG local (lock-free)
        let mut tips = self.dag.find_tips();
        if tips.is_empty() {
            tracing::error!(
                dag_blocks = self.dag.len(),
                "top_tips: ALL sources returned empty! \
                 RAM DAG has {} blocks but 0 tips. \
                 Fee distribution will be blocked.",
                self.dag.len()
            );
        }
        if tips.len() > limit {
            tips.truncate(limit);
        }
        Ok(tips)
    }

    /// Diffuse un bloc sur le reseau **si** un serveur est attache.
    ///
    /// - "Fire-and-forget" : si pas de serveur (ex: mode offline), on ne renvoie pas d'erreur.
    pub(super) async fn do_broadcast_block(&self, _wb: &WireBlock) -> Result<()> {
        // if let Some(srv) = self.server_arc().await {
        //     srv.broadcast(&NetMsg::Block {
        //         id: wb.id.clone(),
        //         parents: wb.parents.clone(),
        //         payload_json: wb.payload_json.clone(),
        //         nonce: wb.nonce,
        //         network_id: wb.network_id.clone(),
        //         protocol_version: wb.protocol_version,
        //         signature_hex: wb.signature_hex.clone(),
        //         signer_pk_hex: wb.signer_pk_hex.clone(),
        //     }).await?;
        // }
        Ok(())
    }
}
