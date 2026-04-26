//! Block processing pipeline: incoming block validation, orphan management, and persistence.

use super::Server;
use pms_network::messages::NetMsg;
use pms_storage::store::PutResult;
use pms_wire::WireBlock;
use std::net::SocketAddr;
use tokio::time::Instant;

impl Server {
    pub async fn process_incoming_blocks(
        &self,
        blocks: impl Into<std::collections::VecDeque<WireBlock>>,
        sa: SocketAddr,
    ) {
        let mut process_queue = blocks.into();

        while let Some(mut wb) = process_queue.pop_front() {
            // Multi-ledger: if block has no network_id, use the server's default
            if wb.network_id.is_empty() {
                wb.network_id = self.network_id.clone();
            }
            if wb.protocol_version == 0 {
                wb.protocol_version = self.protocol_version as u16;
            }
            // Resolve the correct adapter for this block's network
            let block_adapter = self.adapter_for_network(&wb.network_id);

            let was_inflight = self.inflight_fetch.remove(&wb.id).is_some();

            /*
            if !was_inflight && self.seen_inv_recently_and_mark(&wb.id).await {
                continue; // déjà vu en gossip récemment et pas demandé explicitement
            }
            */
            tracing::debug!(
                block_id = %wb.id.get(..8).unwrap_or(&wb.id),
                network = %wb.network_id,
                "processing incoming block"
            );
            if block_adapter.have_block(&wb.id).await {
                continue;
            }

            // OPTIMIZATION: Check parents existence BEFORE persist logic
            // This detects ALL missing parents at once, avoiding round-trips for each one.
            let mut missing_to_fetch = Vec::new();
            let mut missing_any = false;

            for p in &wb.parents {
                if !block_adapter.have_block(p).await {
                    missing_any = true;
                    // If not in orphans, we need to fetch it.
                    // If it IS in orphans, we are already waiting for its parents, so just depend on it.
                    if !self.orphans.contains_key(p) {
                        missing_to_fetch.push(p.clone());
                    }

                    // Register dependency: when 'p' arrives, re-process 'wb' (bounded)
                    if self.parent_dependency.len() < self.max_parent_deps {
                        tracing::debug!(
                            parent = %p.get(..8).unwrap_or(p),
                            child = %wb.id.get(..8).unwrap_or(&wb.id),
                            "adding parent dependency"
                        );
                        self.parent_dependency
                            .entry(p.clone())
                            .or_default()
                            .push(wb.id.clone());
                    }
                }
            }

            if missing_any {
                // SAFETY: Enforce orphan cache bound to prevent memory exhaustion
                if self.orphans.len() >= self.max_orphans {
                    tracing::warn!(
                        "Orphan cache full ({} entries), dropping block {}",
                        self.orphans.len(),
                        wb.id.get(..8).unwrap_or(&wb.id)
                    );
                    continue;
                }

                tracing::debug!(
                    block_id = %wb.id.get(..8).unwrap_or(&wb.id),
                    missing_parents = missing_to_fetch.len(),
                    "block orphaned, missing parents"
                );
                tracing::info!(
                    target = "pms_bench",
                    event = "block_orphaned",
                    block_id = %wb.id,
                    missing_parents = missing_to_fetch.len(),
                    "Block orphaned waiting for parents"
                );
                self.orphans.insert(wb.id.clone(), wb.clone());

                for pid in missing_to_fetch {
                    if self.inflight_fetch.len() < self.max_inflight_requests
                        || self.inflight_fetch.contains_key(&pid)
                    {
                        self.inflight_fetch.insert(pid.clone(), Instant::now());
                        let _ = self.unicast(&sa, NetMsg::GetBlock { id: pid }).await;
                    }
                }
                continue;
            }

            // ====== BENCHMARK: Timer persist ======
            let persist_start = tokio::time::Instant::now();
            // =======================================

            tracing::debug!(
                block_id = %wb.id.get(..8).unwrap_or(&wb.id),
                network = %wb.network_id,
                "calling persist_block"
            );
            let result = block_adapter.persist_block(&wb).await;
            tracing::debug!(?result, "persist_block returned");

            match result {
                Ok(PutResult::Inserted) => {
                    crate::metrics::BLOCKS_PERSISTED
                        .with_label_values(&["main"])
                        .inc();
                    // ====== BENCHMARK: Log bloc validé ======
                    let persist_ms = persist_start.elapsed().as_millis();
                    tracing::info!(
                        target = "pms_bench",
                        event = "block_validated",
                        block_id = %wb.id,
                        persist_ms = persist_ms,
                        "Block persisted"
                    );
                    // =========================================
                    // eprintln!("[SRV] {sa} persist OK id={}", wb.id);
                    let _ = self
                        .broadcast_except(
                            &sa,
                            &NetMsg::Inv {
                                ids: vec![wb.id.clone()],
                            },
                        )
                        .await;

                    // Unblock orphans
                    if let Some((_, children)) = self.parent_dependency.remove(&wb.id) {
                        tracing::debug!(
                            parent = %wb.id.get(..8).unwrap_or(&wb.id),
                            children = children.len(),
                            "unblocking orphan children"
                        );
                        for child_id in children {
                            if let Some((_, child_wb)) = self.orphans.remove(&child_id) {
                                tracing::trace!(
                                    child = %child_id.get(..8).unwrap_or(&child_id),
                                    "queueing unblocked child"
                                );
                                process_queue.push_back(child_wb);
                            } else {
                                tracing::warn!(
                                    child = %child_id.get(..8).unwrap_or(&child_id),
                                    "orphan missing from map"
                                );
                            }
                        }
                    }
                }
                Ok(PutResult::AlreadyExists) => {
                    tracing::debug!(
                        block_id = %wb.id.get(..8).unwrap_or(&wb.id),
                        "block already exists, skipping"
                    );
                    // Check orphans just in case
                    if let Some((_, children)) = self.parent_dependency.remove(&wb.id) {
                        for child_id in children {
                            if let Some((_, child_wb)) = self.orphans.remove(&child_id) {
                                process_queue.push_back(child_wb);
                            }
                        }
                    }
                }
                Ok(PutResult::Rejected(reason)) => {
                    // ====== BENCHMARK: Log rejet ======
                    tracing::info!(
                        target = "pms_bench",
                        event = "block_rejected",
                        block_id = %wb.id,
                        reason = %reason,
                        "Block rejected"
                    );
                    // ==================================
                    tracing::warn!(peer = %sa, block_id = %wb.id, %reason, "persist rejected");

                    // Extract missing parent ID from structured rejection messages
                    let missing_parent_id = extract_missing_parent_id(&reason);
                    if let Some(pid_clean) = missing_parent_id {
                        tracing::debug!(peer = %sa, parent = %pid_clean, "fetching missing parent");

                        // Save orphan & dep (bounded)
                        if self.orphans.len() < self.max_orphans {
                            self.orphans.insert(wb.id.clone(), wb.clone());
                            if self.parent_dependency.len() < self.max_parent_deps {
                                self.parent_dependency
                                    .entry(pid_clean.clone())
                                    .or_default()
                                    .push(wb.id.clone());
                            }
                        }

                        // Request parent — unicast back to the peer that
                        // delivered the orphan block. This peer either has
                        // the parent (we ask it) or doesn't (we'll retry
                        // via global sync later). Pre-fix used `broadcast`
                        // which only fans out to inbound peers, so when
                        // `sa` was an OUTBOUND peer the request went
                        // nowhere and the orphan was stuck forever. The
                        // adjacent line 104 already does this correctly
                        // for the metadata-driven parent fetch path.
                        if self.inflight_fetch.len() < self.max_inflight_requests
                            || self.inflight_fetch.contains_key(&pid_clean)
                        {
                            self.inflight_fetch.insert(pid_clean.clone(), Instant::now());
                            let _ = self.unicast(&sa, NetMsg::GetBlock { id: pid_clean }).await;
                        }
                    } else {
                        crate::metrics::BLOCKS_REJECTED
                            .with_label_values(&["main"])
                            .inc();
                    }
                }
                Err(e) => {
                    tracing::error!(block_id = %wb.id, error = %e, "persist error");
                }
            }
        }
    }

}

fn extract_missing_parent_id(reason: &str) -> Option<String> {
    // Look for "parent" keyword followed by a 64-char hex string
    let parts: Vec<&str> = reason.split_whitespace().collect();
    let idx = parts.iter().position(|&r| r == "parent")?;
    let candidate = parts.get(idx + 1)?;
    let clean = candidate.trim();
    if clean.len() == 64 && clean.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(clean.to_string())
    } else {
        None
    }
}
