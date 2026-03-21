//! Broadcast and unicast message delivery for P2P peers.

use super::Server;
use pms_network::messages::NetMsg;
use crate::limits::SEEN_CAPACITY;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

impl Server {
    /// Worker qui groupe les diffusions par lots pour économiser le réseau.
    pub(super) fn spawn_broadcast_worker(self: Arc<Self>, mut rx: mpsc::Receiver<String>) {
        tokio::spawn(async move {
            let mut buffer = Vec::with_capacity(100);
            let flush_interval = Duration::from_millis(10);
            let max_batch = 100;
            let mut interval = tokio::time::interval(flush_interval);

            loop {
                tokio::select! {
                    biased;  // prioritize recv over tick

                    msg = rx.recv() => {
                        match msg {
                            Some(id) => {
                                buffer.push(id);
                                if buffer.len() >= max_batch {
                                    self.flush_broadcast_buffer(&mut buffer).await;
                                }
                            }
                            None => {
                                // Channel closed, flush remaining and exit
                                if !buffer.is_empty() {
                                    self.flush_broadcast_buffer(&mut buffer).await;
                                }
                                break;
                            }
                        }
                    }
                    _ = interval.tick() => {
                        if !buffer.is_empty() {
                            self.flush_broadcast_buffer(&mut buffer).await;
                        }
                    }
                }
            }
        });
    }

    pub(super) async fn flush_broadcast_buffer(&self, buffer: &mut Vec<String>) {
        if buffer.is_empty() {
            return;
        }
        let ids = std::mem::take(buffer);
        tracing::debug!("Broadcasting Inv batch of {} ids", ids.len());
        let _ = self.broadcast(&NetMsg::Inv { ids }).await;
    }

    pub async fn unicast(&self, sa: &SocketAddr, msg: NetMsg) -> anyhow::Result<()> {
        if let Some(pe) = self.peers.get(sa) {
            let line: Arc<str> = serde_json::to_string(&msg)?.into();
            let _ = pe.tx.send(line).await;
        }
        Ok(())
    }

    /// Diffusion à tous les pairs connectés.
    ///
    /// - Chaque `send` est async ; ici on ne `join` pas pour rester simple (best effort).
    /// - En cas d'erreur (pair lent/parti), on ignore.
    pub async fn broadcast(&self, msg: &NetMsg) -> anyhow::Result<()> {
        if let NetMsg::Block { id, .. } = msg {
            self.mark_inv_seen(id);
        }

        // Serialize once, share Arc<str> to all peers (O(1) clone per peer)
        let line: Arc<str> = serde_json::to_string(msg)?.into();

        // Send only to INBOUND peers - those are the connections where remotes are reading
        // Outbound connections are where WE read from, sending there would go nowhere
        for pe in self.peers.iter() {
            if pe.is_inbound {
                let _ = pe.tx.send(line.clone()).await;
            }
        }

        Ok(())
    }

    /// Variante : broadcast sauf `skip`, only to inbound peers.
    pub async fn broadcast_except(&self, skip: &SocketAddr, msg: &NetMsg) -> anyhow::Result<()> {
        if let NetMsg::Block { id, .. } = msg {
            self.mark_inv_seen(id);
        }

        // Serialize once, share Arc<str> to all peers (O(1) clone per peer)
        let line: Arc<str> = serde_json::to_string(msg)?.into();

        for pe in self.peers.iter() {
            if pe.key() == skip || !pe.is_inbound {
                continue;
            }
            let _ = pe.tx.send(line.clone()).await;
        }
        Ok(())
    }

    /// Déclenche une synchronisation globale (demande les tips à tous les pairs).
    /// Utile pour rattraper d'éventuels blocs orphelins ou lors de la convergence.
    fn mark_inv_seen(&self, id: &str) {
        let mut cache = self.seen_invs.lock().unwrap_or_else(|p| p.into_inner());
        cache.put(id.to_string(), Instant::now());
        while cache.len() > SEEN_CAPACITY {
            cache.pop_lru();
        }
    }
}
