//! Serveur P2P minimal avec diffusion (gossip) de blocs.
//!
//! Objectifs pédagogiques :
//! - Montrer comment structurer un serveur réseau asynchrone avec Tokio
//! - Expliquer pourquoi on a besoin de `Send + Sync + 'static` sur l'adapter
//! - Illustrer un flux lecture/écriture par peer, plus un broadcast simple

mod blocks;
mod broadcast;
mod listener;
mod peer;
mod sync;

use crate::limits::SEEN_CAPACITY;
use crate::rate::TokenBucket;
use dashmap::DashMap;
use lru::LruCache;
use pms_interface::NetDagAdapter;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireBlock;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::Instant;

/// Serveur P2P générique, paramétré par un `DagAdapter`.
///
/// - `adapter` : façade vers la logique DAG/persistance (have_block/persist_block)
/// - `peers`   : map (thread-safe) des pairs connectés -> channel de sortie pour unicast/broadcast
///
/// ⚠️ Contrainte importante : `A: DagAdapter + Send + Sync + 'static`
/// - `Send + Sync` : les tâches Tokio peuvent échanger/partager l'adapter en toute sécurité.
/// - `'static`     : tout ce que `tokio::spawn` capture doit vivre `'static` (arc-clonable et sans
///   emprunts temporaires). Ajouter `'static` sur `A` simplifie ces exigences.
pub struct Server {
    pub(super) adapter: Arc<dyn NetDagAdapter>,
    pub(super) peers: DashMap<SocketAddr, PeerState>,
    pub(super) pong_waiters: DashMap<SocketAddr, oneshot::Sender<()>>,
    node_id: String,                             // ident local
    seen_invs: parking_lot::Mutex<LruCache<String, Instant>>, // LRU pour les Inv (gossip)
    pub(super) inflight_fetch: DashMap<String, Instant>,
    pub(super) orphans: DashMap<String, WireBlock>,
    parent_dependency: DashMap<String, Vec<String>>, // ParentID -> Vec<ChildID>
    pub(super) network_id: String,
    pub(super) protocol_version: u32,
    pub(super) node_wallet: Arc<Wallet>,
    /// Canal pour agréger les diffusions (batching)
    pub(super) broadcast_tx: mpsc::Sender<String>,
    pub(super) allowed_peer_ips: Vec<String>,
    pub(super) strict_whitelist: bool,
    /// Multi-ledger manager (optional). When present, P2P routes blocks
    /// to the correct ledger based on `network_id` in WireBlock metadata.
    pub(super) ledger_mgr: Option<Arc<pms_ledger::LedgerManager>>,
    /// Semaphore limiting concurrent inbound P2P connections.
    pub(super) conn_semaphore: Arc<tokio::sync::Semaphore>,

    // ── Configurable P2P limits (from [p2p] TOML, v0.5.9) ─────────────
    /// Maximum concurrent inbound P2P connections. Default: 256.
    pub(super) max_connections: usize,
    /// Per-peer outbound queue capacity. Default: 2 000.
    pub(super) per_peer_queue_cap: usize,
    /// Maximum orphan blocks in memory. Default: 2 000.
    pub(super) max_orphans: usize,
    /// Maximum in-flight GetBlock requests. Default: 10 000.
    pub(super) max_inflight_requests: usize,
    /// Maximum parent→children dependency entries. Default: 5 000.
    pub(super) max_parent_deps: usize,
}

#[derive(Debug)]
pub(super) struct PeerState {
    /// File de sortie vers ce pair (pre-serialized JSON lines)
    pub(super) tx: mpsc::Sender<Arc<str>>,
    /// Seau à jetons par pair pour limiter le débit (anti-flood)
    pub(super) bucket: TokenBucket,
    /// Compteur d’erreurs de parsing JSON successives
    pub(super) parse_errors: u32,
    /// True if peer connected to us (inbound), false if we connected to them (outbound)
    pub(super) is_inbound: bool,
}

impl Server {
    /// Construit un serveur autour d'un adapter.
    /// On retourne un `Arc<Self>` car on a besoin de cloner le serveur dans les tâches spawnées.
    pub fn new(
        adapter: Arc<dyn NetDagAdapter>,
        network_id: impl Into<String>,
        protocol_version: u32,
        node_wallet: Arc<Wallet>,
        p2p_config: &pms_config::P2pConfig,
        ledger_mgr: Option<Arc<pms_ledger::LedgerManager>>,
    ) -> Arc<Self> {
        let node_id = node_wallet.encoded_public_key();

        // Canal avec buffer pour les IDs à diffuser
        let (broadcast_tx, broadcast_rx) = mpsc::channel(10000);

        let max_connections = p2p_config.max_connections;
        let this = Arc::new(Self {
            adapter,
            peers: DashMap::new(),
            pong_waiters: DashMap::new(),
            node_id: node_id.clone(),
            seen_invs: parking_lot::Mutex::new(LruCache::new(SEEN_CAPACITY.try_into().unwrap())),
            inflight_fetch: DashMap::new(),
            orphans: DashMap::new(),
            parent_dependency: DashMap::new(),
            network_id: network_id.into(),
            protocol_version,
            node_wallet,
            broadcast_tx,
            allowed_peer_ips: p2p_config.allowed_peer_ips.clone(),
            strict_whitelist: p2p_config.strict_whitelist,
            ledger_mgr,
            conn_semaphore: Arc::new(tokio::sync::Semaphore::new(max_connections)),
            max_connections,
            per_peer_queue_cap: p2p_config.per_peer_queue_cap,
            max_orphans: p2p_config.max_orphans,
            max_inflight_requests: p2p_config.max_inflight_requests,
            max_parent_deps: p2p_config.max_parent_deps,
        });

        // Lancement du worker d'agrégation
        this.clone().spawn_broadcast_worker(broadcast_rx);

        this
    }

    /// Crée un Server "API-only" sans broadcast worker ni P2P.
    /// Utilisé pour les routes per-ledger dans le multi-ledger,
    /// où seul l'adapter est nécessaire (pas le réseau P2P).
    ///
    /// Si `shared_broadcast_tx` est fourni, les appels `enqueue_broadcast()`
    /// seront routés vers le worker du serveur principal (P2P).
    pub fn api_only(
        adapter: Arc<dyn NetDagAdapter>,
        network_id: impl Into<String>,
        protocol_version: u32,
        node_wallet: Arc<Wallet>,
        shared_broadcast_tx: Option<mpsc::Sender<String>>,
    ) -> Arc<Self> {
        let node_id = node_wallet.encoded_public_key();
        let broadcast_tx = shared_broadcast_tx.unwrap_or_else(|| {
            // Canal dummy (jamais consommé — pas de broadcast worker)
            let (tx, _rx) = mpsc::channel(1);
            tx
        });

        let defaults = pms_config::P2pConfig::default();
        Arc::new(Self {
            adapter,
            peers: DashMap::new(),
            pong_waiters: DashMap::new(),
            node_id,
            seen_invs: parking_lot::Mutex::new(LruCache::new(SEEN_CAPACITY.try_into().unwrap())),
            inflight_fetch: DashMap::new(),
            orphans: DashMap::new(),
            parent_dependency: DashMap::new(),
            network_id: network_id.into(),
            protocol_version,
            node_wallet,
            broadcast_tx,
            allowed_peer_ips: vec![],
            strict_whitelist: false,
            ledger_mgr: None,
            conn_semaphore: Arc::new(tokio::sync::Semaphore::new(defaults.max_connections)),
            max_connections: defaults.max_connections,
            per_peer_queue_cap: defaults.per_peer_queue_cap,
            max_orphans: defaults.max_orphans,
            max_inflight_requests: defaults.max_inflight_requests,
            max_parent_deps: defaults.max_parent_deps,
        })
        // NOTE: pas de spawn_broadcast_worker ici — API-only
    }

    /// Returns a clone of the broadcast channel sender.
    /// Used by per-ledger API-only servers to share the main server's broadcast worker.
    pub fn broadcast_sender(&self) -> mpsc::Sender<String> {
        self.broadcast_tx.clone()
    }

    pub fn adapter_arc(&self) -> Arc<dyn NetDagAdapter> {
        self.adapter.clone()
    }

    pub fn ledger_manager(&self) -> Option<Arc<pms_ledger::LedgerManager>> {
        self.ledger_mgr.clone()
    }

    // ── Multi-ledger P2P helpers ────────────────────────────────────

    /// Returns the adapter for a specific `network_id` (from ledger manager),
    /// falling back to the default adapter if no match or no ledger manager.
    pub(super) fn adapter_for_network(&self, network_id: &str) -> Arc<dyn NetDagAdapter> {
        if let Some(mgr) = &self.ledger_mgr {
            if let Some(l) = mgr.get_by_network_id(network_id) {
                return l.adapter.clone();
            }
        }
        self.adapter.clone()
    }

    /// Returns all adapters (one per ledger). Falls back to just the default
    /// adapter if no ledger manager is configured.
    pub(super) fn all_adapters(&self) -> Vec<Arc<dyn NetDagAdapter>> {
        if let Some(mgr) = &self.ledger_mgr {
            mgr.list_all()
                .into_iter()
                .map(|l| l.adapter.clone())
                .collect()
        } else {
            vec![self.adapter.clone()]
        }
    }

    /// Checks if any ledger has this block.
    pub(super) async fn have_block_any(&self, id: &str) -> bool {
        for a in self.all_adapters() {
            if a.have_block(id).await {
                return true;
            }
        }
        false
    }

    /// Gets a block from any ledger.
    pub(super) async fn get_block_any(&self, id: &str) -> Option<WireBlock> {
        for a in self.all_adapters() {
            if let Ok(Some(wb)) = a.get_block(id).await {
                return Some(wb);
            }
        }
        None
    }

    /// Gets blocks by IDs, searching across all ledgers.
    pub(super) async fn get_blocks_any(&self, ids: &[String]) -> Vec<WireBlock> {
        if self.ledger_mgr.is_none() {
            return self
                .adapter
                .get_blocks_by_ids(ids)
                .await
                .unwrap_or_default();
        }
        let mut result = Vec::new();
        for a in self.all_adapters() {
            if let Ok(blocks) = a.get_blocks_by_ids(ids).await {
                result.extend(blocks);
            }
        }
        result
    }

    /// Aggregates tips from all ledgers.
    pub(super) async fn all_tips(&self, limit: usize) -> Vec<String> {
        if self.ledger_mgr.is_none() {
            return self.adapter.top_tips(limit).await.unwrap_or_default();
        }
        let mut all = Vec::new();
        for a in self.all_adapters() {
            if let Ok(tips) = a.top_tips(limit).await {
                all.extend(tips);
            }
        }
        all
    }

    /// Ajoute un ID de bloc à la file de diffusion.
    /// Il sera groupé avec d'autres IDs pour optimiser le réseau.
    pub async fn enqueue_broadcast(&self, id: String) {
        if let Err(e) = self.broadcast_tx.send(id).await {
            tracing::warn!("enqueue_broadcast: channel send failed: {}", e);
        }
    }

    pub fn node_identity_wallet(&self) -> Arc<Wallet> {
        self.node_wallet.clone()
    }

    /// Retourne la liste des adresses des pairs P2P connectés
    pub fn get_p2p_peers(&self) -> Vec<String> {
        self.peers.iter().map(|p| p.key().to_string()).collect()
    }

    pub(super) fn is_peer_allowed(&self, addr: &SocketAddr) -> bool {
        if self.allowed_peer_ips.is_empty() {
            return true;
        }
        let ip_str = addr.ip().to_string();
        self.allowed_peer_ips
            .iter()
            .any(|allowed| allowed == &ip_str || allowed == "*")
    }

}
