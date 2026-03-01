//! Serveur P2P minimal avec diffusion (gossip) de blocs.
//!
//! Objectifs pédagogiques :
//! - Montrer comment structurer un serveur réseau asynchrone avec Tokio
//! - Expliquer pourquoi on a besoin de `Send + Sync + 'static` sur l’adapter
//! - Illustrer un flux lecture/écriture par peer, plus un broadcast simple

use crate::api;
use crate::limits::{
    HANDSHAKE_TIMEOUT_MS, MAX_BLOCKS_BATCH, MAX_INFLIGHT_GETBLOCK, MAX_LINE_BYTES, MAX_ORPHANS,
    MAX_PARENT_DEPS, MAX_PARSE_ERRORS, PER_PEER_Q_CAP, PING_EVERY_MS, RATE_BURST,
    RATE_MSGS_PER_SEC, SEEN_CAPACITY,
};
use crate::rate::TokenBucket;
use crate::stats::Stats;
use dashmap::DashMap;
use lru::LruCache;
use pms_config::{ServerConfig, TlsConfig};
use pms_interface::NetDagAdapter;
use pms_network::messages::NetMsg;
use pms_storage::rocks_store::store::RocksStore;
use pms_storage::store::PutResult;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireBlock;
use rand::random;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::{net::SocketAddr, sync::Arc};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, oneshot};
use tokio::time::{Instant, sleep};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

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
    adapter: Arc<dyn NetDagAdapter>,
    peers: DashMap<SocketAddr, PeerState>,
    pong_waiters: DashMap<SocketAddr, oneshot::Sender<()>>,
    node_id: String,                             // ident local
    seen_invs: Mutex<LruCache<String, Instant>>, // LRU pour les Inv (gossip)
    inflight_fetch: Mutex<HashMap<String, Instant>>,
    orphans: DashMap<String, WireBlock>,
    parent_dependency: DashMap<String, Vec<String>>, // ParentID -> Vec<ChildID>
    network_id: String,
    protocol_version: u32,
    node_wallet: Arc<Wallet>,
    /// Canal pour agréger les diffusions (batching)
    broadcast_tx: mpsc::Sender<String>,
    allowed_peer_ips: Vec<String>,
    strict_whitelist: bool,
    /// Multi-ledger manager (optional). When present, P2P routes blocks
    /// to the correct ledger based on `network_id` in WireBlock metadata.
    ledger_mgr: Option<Arc<pms_ledger::LedgerManager>>,
}

#[derive(Debug)]
struct PeerState {
    /// File de sortie vers ce pair (messages que NOUS lui envoyons)
    tx: mpsc::Sender<NetMsg>,
    /// Seau à jetons par pair pour limiter le débit (anti-flood)
    bucket: TokenBucket,
    /// Compteur d’erreurs de parsing JSON successives
    parse_errors: u32,
    /// True if peer connected to us (inbound), false if we connected to them (outbound)
    is_inbound: bool,
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

        let this = Arc::new(Self {
            adapter,
            peers: DashMap::new(),
            pong_waiters: DashMap::new(),
            node_id: node_id.clone(),
            seen_invs: Mutex::new(LruCache::new(SEEN_CAPACITY.try_into().unwrap())),
            inflight_fetch: Mutex::new(HashMap::new()),
            orphans: DashMap::new(),
            parent_dependency: DashMap::new(),
            network_id: network_id.into(),
            protocol_version,
            node_wallet,
            broadcast_tx,
            allowed_peer_ips: p2p_config.allowed_peer_ips.clone(),
            strict_whitelist: p2p_config.strict_whitelist,
            ledger_mgr,
        });

        // Lancement du worker d'agrégation
        this.clone().spawn_broadcast_worker(broadcast_rx);

        this
    }

    /// Worker qui groupe les diffusions par lots pour économiser le réseau.
    fn spawn_broadcast_worker(self: Arc<Self>, mut rx: mpsc::Receiver<String>) {
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

    async fn flush_broadcast_buffer(&self, buffer: &mut Vec<String>) {
        if buffer.is_empty() {
            return;
        }
        let ids = std::mem::take(buffer);
        tracing::debug!("Broadcasting Inv batch of {} ids", ids.len());
        let _ = self.broadcast(&NetMsg::Inv { ids }).await;
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

        Arc::new(Self {
            adapter,
            peers: DashMap::new(),
            pong_waiters: DashMap::new(),
            node_id,
            seen_invs: Mutex::new(LruCache::new(SEEN_CAPACITY.try_into().unwrap())),
            inflight_fetch: Mutex::new(HashMap::new()),
            orphans: DashMap::new(),
            parent_dependency: DashMap::new(),
            network_id: network_id.into(),
            protocol_version,
            node_wallet,
            broadcast_tx,
            allowed_peer_ips: vec![],
            strict_whitelist: false,
            ledger_mgr: None,
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
    fn adapter_for_network(&self, network_id: &str) -> Arc<dyn NetDagAdapter> {
        if let Some(mgr) = &self.ledger_mgr {
            if let Some(l) = mgr.get_by_network_id(network_id) {
                return l.adapter.clone();
            }
        }
        self.adapter.clone()
    }

    /// Returns all adapters (one per ledger). Falls back to just the default
    /// adapter if no ledger manager is configured.
    fn all_adapters(&self) -> Vec<Arc<dyn NetDagAdapter>> {
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
    async fn have_block_any(&self, id: &str) -> bool {
        for a in self.all_adapters() {
            if a.have_block(id).await {
                return true;
            }
        }
        false
    }

    /// Gets a block from any ledger.
    async fn get_block_any(&self, id: &str) -> Option<WireBlock> {
        for a in self.all_adapters() {
            if let Ok(Some(wb)) = a.get_block(id).await {
                return Some(wb);
            }
        }
        None
    }

    /// Gets blocks by IDs, searching across all ledgers.
    async fn get_blocks_any(&self, ids: &[String]) -> Vec<WireBlock> {
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
    async fn all_tips(&self, limit: usize) -> Vec<String> {
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

    fn is_peer_allowed(&self, addr: &SocketAddr) -> bool {
        if self.allowed_peer_ips.is_empty() {
            return true;
        }
        let ip_str = addr.ip().to_string();
        self.allowed_peer_ips
            .iter()
            .any(|allowed| allowed == &ip_str || allowed == "*")
    }

    /// Connect to a peer dynamically (Outbound)
    pub async fn connect_to_peer(
        self: Arc<Self>,
        addr_str: String,
        tls_config: Option<TlsConfig>,
    ) -> anyhow::Result<()> {
        // Resolve hostname (e.g. "node1:8080" -> 172.18.0.3:8080)
        let addr = tokio::net::lookup_host(&addr_str)
            .await
            .map_err(|e| anyhow::anyhow!("DNS Lookup failed for '{}': {}", addr_str, e))?
            .next()
            .ok_or_else(|| {
                anyhow::anyhow!("Could not resolve address: {} (no results)", addr_str)
            })?;

        if let Some(tls) = tls_config {
            use rustls::pki_types::{IpAddr, ServerName};

            let client_config =
                crate::tls::load_client_config(&tls.cert_pem, &tls.key_pem, tls.ca_pem.as_deref())?;
            let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));

            // SNI: If it looks like an IP, use IpAddress, otherwise DnsName
            let host_part = addr_str.split(':').next().unwrap_or(&addr_str);
            let domain = if let Ok(ip_addr) = host_part.parse::<std::net::IpAddr>() {
                let sni_ip = match ip_addr {
                    std::net::IpAddr::V4(ip) => IpAddr::V4(ip.into()),
                    std::net::IpAddr::V6(ip) => IpAddr::V6(ip.into()),
                };
                ServerName::IpAddress(sni_ip)
            } else {
                ServerName::try_from(host_part)
                    .map_err(|_| anyhow::anyhow!("Invalid DNS name: {}", host_part))?
                    .to_owned()
            };

            tracing::info!(
                "🔌 Connecting TLS to {} ({:?}) SNI={:?}",
                addr_str,
                addr,
                domain
            );

            let stream = TcpStream::connect(addr).await.map_err(|e| {
                tracing::error!("❌ TCP Connect failed to {}: {}", addr, e);
                e
            })?;
            tracing::info!("✅ TCP Connected to {}", addr);

            let tls_stream = connector.connect(domain, stream).await.map_err(|e| {
                tracing::error!("❌ TLS Handshake failed to {}: {}", addr, e);
                e
            })?;
            tracing::info!("✅ TLS Handshake success with {}", addr);

            let (r, w) = tokio::io::split(tls_stream);
            self.handle_new_peer_from_io(r, w, addr, false).await?;
        } else {
            tracing::info!("🔌 Connecting TCP to {} (No TLS)", addr);
            let stream = TcpStream::connect(addr).await?;
            let (r, w) = tokio::io::split(stream);
            self.handle_new_peer_from_io(r, w, addr, false).await?;
        }
        Ok(())
    }

    pub async fn run(
        self: Arc<Self>,
        cfg: Arc<ServerConfig>,
        store: Arc<RocksStore>,
    ) -> anyhow::Result<()> {
        let stats = Arc::new(Stats::new());
        let ready = Arc::new(AtomicBool::new(false));

        tracing::info!(target="pms_stats", ptr=?Arc::as_ptr(&stats), "stats_ptr run()");
        tracing::info!("🔑 Local Node Identity: {}", self.node_id);

        // Note: PMS_BLOCKS_TOTAL gauge is synced from dag.len() on each /metrics fetch.
        // No init needed here — the first metrics poll will set the correct value.

        // 1) Logger périodique des stats (persist / gossip)
        {
            let stats = stats.clone();
            let srv_for_sync = self.clone();
            tokio::spawn(async move {
                loop {
                    sleep(Duration::from_millis(200)).await; // Super-Aggressive sync (200ms)
                    // 1) Log stats (Throttle logs to every 2s to avoid spam)
                    // ... actually we can just log every time or use a counter.
                    // Let's keep it simple: log every iteration but the loop is fast.
                    // Wait, logging every 200ms might spam. Let's use a counter.

                    let (ok, dup, err, go, gr, ge) = stats.snapshot();
                    if random::<u8>() < 25 {
                        // Log roughly every ~8-10 iterations (~2s)
                        tracing::info!(
                            target="pms_stats",
                            ptr=?Arc::as_ptr(&stats),
                            "📊 200ms: persist ok={} dup={} err={} | gossip ok={} reject={} err={}",
                            ok, dup, err, go, gr, ge
                        );
                    }

                    // 2) Retry Missing Parents for Orphans
                    {
                        let parents_needed: Vec<String> = srv_for_sync
                            .parent_dependency
                            .iter()
                            .map(|entry| entry.key().clone())
                            .collect();

                        if !parents_needed.is_empty() {
                            tracing::info!(
                                "🔄 Retrying {} missing parents for orphans",
                                parents_needed.len()
                            );

                            let mut to_fetch = Vec::new();
                            {
                                let mut inflight = srv_for_sync.inflight_fetch.lock().await;
                                for pid in parents_needed {
                                    if let Some(ts) = inflight.get(&pid) {
                                        if ts.elapsed().as_millis() < 2000 {
                                            continue;
                                        }
                                    }
                                    inflight.insert(pid.clone(), Instant::now());
                                    to_fetch.push(pid);
                                    if to_fetch.len() >= 100 {
                                        break;
                                    }
                                }
                            }

                            if !to_fetch.is_empty() {
                                if to_fetch.len() == 1 {
                                    let _ = srv_for_sync
                                        .broadcast(&NetMsg::GetBlock {
                                            id: to_fetch[0].clone(),
                                        })
                                        .await;
                                } else {
                                    let _ = srv_for_sync
                                        .broadcast(&NetMsg::GetBlocks { ids: to_fetch })
                                        .await;
                                }
                            }
                        }
                    }

                    // 3) Active Sync: Broadcast GetTips with higher limit
                    // Limit 1024 covers extremely wide DAGs (stress tests)
                    let _ = srv_for_sync
                        .broadcast(&NetMsg::GetTips { limit: 1024 })
                        .await;
                }
            });
        }

        // 2) Démarrage de l'API HTTP (TLS ou non, la logique est dans run_api)
        {
            let srv = self.clone();
            let addr = cfg.api_addr.clone();
            let cfg_for_api = cfg.clone();
            let stats_for_api = stats.clone();
            let ready_for_api = ready.clone();
            let store_for_api = store.clone();

            let api_handle = tokio::spawn(async move {
                srv.run_api(
                    addr,
                    cfg_for_api,
                    ready_for_api,
                    stats_for_api,
                    store_for_api,
                )
                .await
            });

            // Watchdog: detect API task crash (panic or error) and exit process
            tokio::spawn(async move {
                match api_handle.await {
                    Ok(Ok(())) => {
                        eprintln!("[API] Server exited cleanly (unexpected)");
                        std::process::exit(1);
                    }
                    Ok(Err(e)) => {
                        eprintln!("[API] FATAL error: {e}");
                        std::process::exit(1);
                    }
                    Err(join_err) => {
                        eprintln!("[API] PANIC in API server task: {:?}", join_err);
                        std::process::exit(1);
                    }
                }
            });
        }

        // 3) Tâches de maintenance RocksDB (flush / compaction / stats)
        let cancel = CancellationToken::new();
        let maint_handle = store.spawn_background_maintenance(
            cancel.clone(),
            Duration::from_secs(3600), // compact toutes les 1h
            Duration::from_secs(600),  // flush WAL toutes les 10 min
            Duration::from_secs(1800), // stats toutes les 30 min
        );

        // 4) Marque le nœud comme "ready" pour /ready
        ready.store(true, Ordering::Relaxed);

        // 5) Listener P2P (TLS ou TCP clair selon config + présence des fichiers)
        let bind_addr: SocketAddr = cfg.bind_addr.parse()?;
        let mode = &cfg.network.mode; // Dev / Testnet / Mainnet

        let res = if let Some(tls) = cfg.tls.clone() {
            let cert_exists = Path::new(&tls.cert_pem).exists();
            let key_exists = Path::new(&tls.key_pem).exists();

            if mode.is_prod() {
                // 🔐 En prod: TLS obligatoire si configuré, et fichiers requis
                if !cert_exists || !key_exists {
                    anyhow::bail!(
                        "[P2P] TLS activé en {:?} mais cert/key introuvables: cert={} key={}",
                        mode,
                        tls.cert_pem,
                        tls.key_pem
                    );
                }
                eprintln!("[P2P] {:?} + TLS → listen_tls({})", mode, bind_addr);
                self.listen_tls(&cfg.bind_addr, tls).await
            } else {
                // 🧪 Dev / Testnet: on est tolérant
                if !cert_exists || !key_exists {
                    eprintln!(
                        "[P2P] TLS configuré mais fichiers absents en mode {:?}, fallback TCP clair sur {}",
                        mode, bind_addr
                    );
                    self.listen(&cfg.bind_addr).await
                } else {
                    eprintln!("[P2P] mode {:?} avec TLS → listen_tls({})", mode, bind_addr);
                    self.listen_tls(&cfg.bind_addr, tls).await
                }
            }
        } else {
            // Pas de bloc [tls] → P2P en clair
            eprintln!("[P2P] aucun TLS configuré → listen({})", bind_addr);
            self.listen(&cfg.bind_addr).await
        };

        // 6) Arrêt propre des tâches de maintenance
        cancel.cancel();
        let _ = maint_handle.await;

        res
    }

    async fn run_api(
        self: Arc<Self>,
        addr: String,
        cfg: Arc<ServerConfig>,
        ready: Arc<AtomicBool>,
        stats: Arc<Stats>,
        store: Arc<RocksStore>,
    ) -> anyhow::Result<()> {
        api::serve_api(&addr, self, cfg, ready, stats, store).await?;
        Ok(())
    }

    pub async fn listen_tls(self: Arc<Self>, bind: &str, tls: TlsConfig) -> anyhow::Result<()> {
        use tokio::net::TcpListener;
        use tokio_rustls::TlsAcceptor;

        let addr: std::net::SocketAddr = bind.parse()?;
        let listener = TcpListener::bind(addr).await?;

        // charge cert + clé (PKCS#8 OU EC)
        let sc = crate::tls::load_tls(&tls.cert_pem, &tls.key_pem)?;
        let acceptor = TlsAcceptor::from(std::sync::Arc::new(sc));

        tracing::info!("P2P TLS listening on {addr}");

        loop {
            let (tcp, sa) = listener.accept().await?;
            let acceptor = acceptor.clone();
            let this = Arc::clone(&self);

            tokio::spawn(async move {
                match acceptor.accept(tcp).await {
                    Ok(tls_stream) => {
                        if let Err(e) = this.handle_new_peer_tls(tls_stream, sa).await {
                            eprintln!("[P2P/TLS] {sa} error: {e}");
                        }
                    }
                    Err(e) => eprintln!("[P2P/TLS] accept failed from {sa}: {e}"),
                }
            });
        }
    }

    /// Démarre l’écoute TCP sur `addr` (ex: "0.0.0.0:8050") et boucle pour accepter les connexions.
    ///
    /// Pour chaque connexion :
    /// - on crée un channel mpsc (sortie) pour pouvoir envoyer des messages vers ce pair
    /// - on spawn 2 tâches :
    ///     1) tâche **écriture** : consomme le channel et écrit sur le socket
    ///     2) tâche **lecture**  : lit ligne par ligne (JSONL), traite les messages, gossip des blocs
    pub async fn listen(self: Arc<Self>, addr: &str) -> anyhow::Result<()> {
        let lis = TcpListener::bind(addr).await?;
        loop {
            let (stream, sa) = lis.accept().await?;
            self.handle_new_peer(stream, sa).await?;
        }
    }

    pub async fn listen_ready(
        self: Arc<Self>,
        addr: &str,
        ready: tokio::sync::oneshot::Sender<()>,
    ) -> anyhow::Result<()> {
        let lis = TcpListener::bind(addr).await?;
        let _ = ready.send(()); // ✅ signal "bind OK"
        loop {
            let (stream, sa) = lis.accept().await?;
            self.handle_new_peer(stream, sa).await?;
        }
    }

    /// Gère l’initialisation d’un pair : installe les tâches lecture/écriture.
    async fn handle_new_peer(
        self: &Arc<Self>,
        stream: TcpStream,
        sa: SocketAddr,
    ) -> anyhow::Result<()> {
        if self.strict_whitelist && !self.is_peer_allowed(&sa) {
            tracing::warn!("🚫 Rejected P2P connection from {} (not in whitelist)", sa);
            return Ok(());
        }
        let (r, w) = tokio::io::split(stream);
        self.handle_new_peer_from_io(r, w, sa, true).await // inbound = true
    }

    pub async fn handle_new_peer_tls(
        self: &Arc<Self>,
        tls_stream: tokio_rustls::server::TlsStream<TcpStream>,
        sa: SocketAddr,
    ) -> anyhow::Result<()> {
        let (r, w) = tokio::io::split(tls_stream);
        self.handle_new_peer_from_io(r, w, sa, true).await // inbound = true
    }

    /// Handles the initialization and management of a new peer connection.
    ///
    /// This function performs tasks associated with managing a new peer that connects to the server,
    /// including setup for incoming (read) and outgoing (write) I/O streams, initial handshake, and
    /// communication management.
    ///
    /// # Arguments
    /// - `reader_io`: The reader that manages input from the connected peer. It must implement `AsyncRead`, `Unpin`,
    ///   `Send`, and have a static lifetime.
    /// - `writer_io`: The writer that manages output to the connected peer. It must implement `AsyncWrite`, `Unpin`,
    ///   `Send`, and have a static lifetime.
    /// - `sa`: The socket address (IP and port) associated with the peer connection.
    ///
    /// # Return
    /// Returns an `anyhow::Result<()>` that indicates success or any error that occurred during the handling
    /// of the peer connection.
    ///
    /// # Behavior
    /// - Initializes a bounded MPSC channel for outgoing messages to the peer.
    /// - Registers the peer connection in the server's peer state (`PeerState`), including:
    ///     - A token bucket for rate-limiting messages.
    ///     - A counter for tracking parse errors.
    ///     - A timestamp for the last activity with the peer.
    /// - Sends an initial `Hello` message to the peer over a unicast channel.
    ///
    /// - Spawns two asynchronous tasks:
    ///     1. **Write Task:**
    ///         - Listens for outgoing messages (`NetMsg`) from the MPSC channel.
    ///         - Serializes messages to JSON and writes them to the output stream.
    ///         - Handles graceful stream shutdown and peer removal from the state when the peer disconnects.
    ///     2. **Read Task:**
    ///         - Reads incoming messages line-by-line using a buffered reader (`BufReader`).
    ///         - Enforces a handshake protocol to ensure peers communicate with the correct protocol version (`proto == 1`).
    ///         - Disconnects peers that violate expected patterns (e.g., overly long messages, too many parse errors, or
    ///           sending messages before a handshake is completed).
    ///         - Processes specific received `NetMsg` messages:
    ///             - **Ping:** Responds with `Pong` while respecting a rate limit.
    ///             - **Pong:** Handles Pong acknowledgments for tracking peer latency.
    ///             - **Block:** Validates and persists new blocks, broadcasting valid blocks to other peers.
    ///             - **Hello / HelloAck:** Handles the handshake process to establish or accept a connection, requesting
    ///               additional tips if needed.
    ///             - Any other invalid or unexpected messages cause the peer to be disconnected.
    ///
    /// # Error Handling
    /// - Gracefully handles invalid or unexpected inputs:
    ///     - Peers that send messages larger than `MAX_LINE_BYTES` are disconnected.
    ///     - Peers that exceed `MAX_PARSE_ERRORS` are disconnected.
    ///     - If the handshake is not completed within `HANDSHAKE_TIMEOUT_MS`, the connection is terminated.
    /// - Logs detailed error and connection status information for debugging and monitoring.
    ///
    /// # Note
    /// - This function uses `tokio::spawn` to manage the read and write tasks for the peer in parallel.
    /// - Removes peers from the server's state (`self.peers`) when they disconnect or are forcibly removed.
    /// - Implements basic anti-abuse measures such as rate-limiting and error thresholds.
    pub async fn handle_new_peer_from_io<R, W>(
        self: &Arc<Self>,
        reader_io: R,
        mut writer_io: W,
        sa: SocketAddr,
        is_inbound: bool,
    ) -> anyhow::Result<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        // On encapsule le reader dans un BufReader pour faire des `read_line` efficaces.
        let (tx_out, mut rx_out) = mpsc::channel::<NetMsg>(PER_PEER_Q_CAP);

        self.peers.insert(
            sa,
            PeerState {
                tx: tx_out,
                bucket: TokenBucket::new(RATE_MSGS_PER_SEC, RATE_BURST),
                parse_errors: 0,
                is_inbound,
            },
        );

        let hello = NetMsg::Hello {
            proto: 1,
            node_id: self.node_id.clone(),
            nonce: random::<u64>(),
            ping_ms: PING_EVERY_MS,
        };
        let _ = self.unicast(&sa, hello).await;

        let this_w = Arc::clone(self);
        tokio::spawn(async move {
            while let Some(m) = rx_out.recv().await {
                if let Ok(s) = serde_json::to_string(&m) {
                    if writer_io.write_all(s.as_bytes()).await.is_err() {
                        break;
                    }
                    if writer_io.write_all(b"\n").await.is_err() {
                        break;
                    }
                    // CRITICAL: Flush the TLS buffer to actually send the data
                    if writer_io.flush().await.is_err() {
                        break;
                    }
                }
            }
            let _ = writer_io.shutdown().await;
            this_w.peers.remove(&sa);
        });

        let mut handshaked = false;
        let handshake_deadline = Instant::now() + Duration::from_millis(HANDSHAKE_TIMEOUT_MS);
        let mut reader = tokio::io::BufReader::new(reader_io);

        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut line = String::new();
            // Idle timeout: disconnect peers that send nothing for 60s
            let idle_timeout = Duration::from_secs(60);

            loop {
                let read_result =
                    tokio::time::timeout(idle_timeout, reader.read_line(&mut line)).await;

                let bytes_read = match read_result {
                    Ok(Ok(n)) if n > 0 => n,
                    Ok(Ok(_)) => break,  // EOF
                    Ok(Err(_)) => break, // Read error
                    Err(_) => {
                        // Timeout: peer idle too long
                        tracing::debug!("Peer {} idle timeout ({}s)", sa, idle_timeout.as_secs());
                        this.peers.remove(&sa);
                        break;
                    }
                };
                let _ = bytes_read;
                {
                    if !handshaked && Instant::now() > handshake_deadline {
                        this.peers.remove(&sa);
                        break;
                    }

                    if line.len() > MAX_LINE_BYTES {
                        this.peers.remove(&sa);
                        break;
                    }

                    let parsed = serde_json::from_str::<NetMsg>(&line);
                    if parsed.is_err() {
                        if let Some(mut pe) = this.peers.get_mut(&sa) {
                            pe.parse_errors = pe.parse_errors.saturating_add(1);
                            if pe.parse_errors > MAX_PARSE_ERRORS {
                                break;
                            }
                        }
                        line.clear();
                        continue;
                    }
                    let msg = parsed.unwrap();

                    // Add general log for incoming message type
                    println!("[SRV] {} -> Recv Msg: {:?}", sa, msg);

                    if !handshaked {
                        match msg {
                            NetMsg::Hello { proto, node_id, .. } => {
                                if proto != 1 {
                                    let _ = this
                                        .unicast(
                                            &sa,
                                            NetMsg::HelloAck {
                                                ok: false,
                                                reason: Some("bad proto".into()),
                                            },
                                        )
                                        .await;
                                    this.peers.remove(&sa);
                                    break;
                                }
                                if node_id == this.node_id {
                                    let _ = this
                                        .unicast(
                                            &sa,
                                            NetMsg::HelloAck {
                                                ok: false,
                                                reason: Some("loopback".into()),
                                            },
                                        )
                                        .await;
                                    this.peers.remove(&sa);
                                    break;
                                }
                                let _ = this
                                    .unicast(
                                        &sa,
                                        NetMsg::HelloAck {
                                            ok: true,
                                            reason: None,
                                        },
                                    )
                                    .await;
                                handshaked = true;
                                let _ = this.unicast(&sa, NetMsg::GetTips { limit: 64 }).await;
                                line.clear();
                                continue;
                            }
                            NetMsg::HelloAck { ok, .. } => {
                                if !ok {
                                    this.peers.remove(&sa);
                                    break;
                                }
                                handshaked = true;
                                let _ = this.unicast(&sa, NetMsg::GetTips { limit: 64 }).await;
                                line.clear();
                                continue;
                            }
                            _ => {
                                this.peers.remove(&sa);
                                break;
                            }
                        }
                    }

                    match msg {
                        NetMsg::Ping => {
                            let mut allow = false;
                            if let Some(mut pe) = this.peers.get_mut(&sa) {
                                allow = pe.bucket.take(1);
                            }
                            if allow {
                                let _ = this.unicast(&sa, NetMsg::Pong).await;
                            }
                        }
                        NetMsg::Pong => {
                            if let Some((_, waiter)) = this.pong_waiters.remove(&sa) {
                                let _ = waiter.send(());
                            }
                        }
                        NetMsg::Block {
                            id,
                            parents,
                            payload_json,
                            nonce,
                            network_id,
                            protocol_version,
                            signer_pk_hex,
                            signature_hex,
                            metadata,
                        } => {
                            let wb = WireBlock {
                                id,
                                parents,
                                payload_json,
                                nonce,
                                network_id,
                                protocol_version,
                                signer_pk_hex,
                                signature_hex,
                                metadata,
                            };
                            this.process_incoming_blocks(vec![wb], sa).await;
                        }
                        NetMsg::Inv { ids } => {
                            eprintln!("[SRV] {} <- Inv({} ids)", sa, ids.len());
                            let mut to_fetch = Vec::with_capacity(ids.len());
                            for id in ids {
                                // Check all ledgers (multi-ledger aware)
                                if this.have_block_any(&id).await {
                                    continue;
                                }
                                // Check gossip cache - DISABLED: was causing premature filtering
                                // The inflight_fetch check below is sufficient to prevent spam
                                // if this.seen_inv_recently_and_mark(&id).await {
                                //     continue;
                                // }
                                eprintln!(
                                    "[SRV] Processing Inv ID: {}",
                                    id.get(..8).unwrap_or(&id)
                                );
                                let mut inflight = this.inflight_fetch.lock().await;
                                if let Some(ts) = inflight.get(&id) {
                                    if ts.elapsed().as_millis() < crate::limits::INFLIGHT_TTL_MS {
                                        continue;
                                    }
                                }
                                if inflight.len() < MAX_INFLIGHT_GETBLOCK {
                                    inflight.insert(id.clone(), Instant::now());
                                    to_fetch.push(id.clone()); // Log clone
                                    eprintln!("[SRV] Requesting {} from {}", id, sa);
                                } else {
                                    break;
                                }
                            }
                            if !to_fetch.is_empty() {
                                if to_fetch.len() == 1 {
                                    let _ = this
                                        .broadcast(&NetMsg::GetBlock {
                                            id: to_fetch[0].clone(),
                                        })
                                        .await;
                                } else {
                                    let _ =
                                        this.broadcast(&NetMsg::GetBlocks { ids: to_fetch }).await;
                                }
                            }
                        }
                        NetMsg::GetBlock { id } => {
                            // Search across all ledgers
                            if let Some(wb) = this.get_block_any(&id).await {
                                let _ =
                                    this.unicast(&sa, NetMsg::Blocks { blocks: vec![wb] }).await;
                            }
                        }
                        NetMsg::GetBlocks { ids } => {
                            eprintln!("[SRV] {sa} -> GetBlocks({} ids)", ids.len());
                            let blocks = this.get_blocks_any(&ids).await;
                            if !blocks.is_empty() {
                                eprintln!("[SRV] Sending {} blocks to {}", blocks.len(), sa);
                                let _ = this.unicast(&sa, NetMsg::Blocks { blocks }).await;
                            } else {
                                eprintln!("[SRV] GetBlocks returned empty for {} ids", ids.len());
                            }
                        }
                        NetMsg::Blocks { mut blocks } => {
                            if blocks.len() > MAX_BLOCKS_BATCH {
                                blocks.truncate(MAX_BLOCKS_BATCH);
                            }
                            this.process_incoming_blocks(blocks, sa).await;
                        }
                        NetMsg::GetTips { limit } => {
                            // Aggregate tips from all ledgers
                            let ids = this.all_tips(limit).await;
                            println!("[SRV] Serving GetTips: {} tips", ids.len());
                            let _ = this.unicast(&sa, NetMsg::Tips { ids }).await;
                        }
                        NetMsg::Tips { ids } => {
                            let mut to_fetch = Vec::new();
                            println!("[SRV] Processing Tips: {} ids", ids.len());
                            for id in ids {
                                // Check all ledgers
                                if this.have_block_any(&id).await {
                                    continue;
                                }
                                // BUG FIX: Don't check seen_inv for Tips!
                                // Tips are authoritative sync info. If we don't have the block and it's not inflight,
                                // we must fetch it, even if we saw an Inv recently (e.g. failed fetch).
                                // if this.seen_inv_recently_and_mark(&id).await { countinue; }
                                let mut inflight = this.inflight_fetch.lock().await;
                                let in_inflight = inflight.contains_key(&id);
                                println!(
                                    "[SRV] Tips {}: have=false, inflight={}",
                                    id.get(..8).unwrap_or(&id),
                                    in_inflight
                                );

                                if let Some(ts) = inflight.get(&id) {
                                    if ts.elapsed().as_millis() < crate::limits::INFLIGHT_TTL_MS {
                                        continue;
                                    }
                                }
                                if inflight.len() < MAX_INFLIGHT_GETBLOCK {
                                    inflight.insert(id.clone(), Instant::now());
                                    to_fetch.push(id);
                                }
                            }
                            if !to_fetch.is_empty() {
                                println!("[SRV] Sending GetBlocks for {} items", to_fetch.len());
                                let _ =
                                    this.unicast(&sa, NetMsg::GetBlocks { ids: to_fetch }).await;
                            }
                        }
                        NetMsg::Hello { .. } | NetMsg::HelloAck { .. } => {}
                    }
                    line.clear();
                } // end inner block
            } // end loop
            this.pong_waiters.remove(&sa);
        });

        Ok(())
    }

    /// Envoi d’un message à un pair précis.
    ///
    /// - On retrouve le `Sender` dans la map et on pousse le message.
    /// - Si la file est pleine ou le pair déjà parti, on ignore l’erreur (best effort).
    pub async fn unicast(&self, sa: &SocketAddr, msg: NetMsg) -> anyhow::Result<()> {
        if let Some(pe) = self.peers.get(sa) {
            let _ = pe.tx.send(msg).await; // <-- pe.tx (plus .value().tx)
        }
        Ok(())
    }

    /// Diffusion à tous les pairs connectés.
    ///
    /// - Chaque `send` est async ; ici on ne `join` pas pour rester simple (best effort).
    /// - En cas d’erreur (pair lent/parti), on ignore.
    pub async fn broadcast(&self, msg: &NetMsg) -> anyhow::Result<()> {
        if let NetMsg::Block { id, .. } = msg {
            self.mark_inv_seen(id).await;
        }

        // Send only to INBOUND peers - those are the connections where remotes are reading
        // Outbound connections are where WE read from, sending there would go nowhere
        for pe in self.peers.iter() {
            if pe.is_inbound {
                let _ = pe.tx.send(msg.clone()).await;
            }
        }

        Ok(())
    }

    /// Variante : broadcast sauf `skip`, only to inbound peers.
    pub async fn broadcast_except(&self, skip: &SocketAddr, msg: &NetMsg) -> anyhow::Result<()> {
        if let NetMsg::Block { id, .. } = msg {
            self.mark_inv_seen(id).await;
        }
        for pe in self.peers.iter() {
            if pe.key() == skip || !pe.is_inbound {
                continue;
            }
            let _ = pe.tx.send(msg.clone()).await;
        }
        Ok(())
    }

    /// Déclenche une synchronisation globale (demande les tips à tous les pairs).
    /// Utile pour rattraper d'éventuels blocs orphelins ou lors de la convergence.
    pub async fn trigger_sync(&self) {
        // 1) Cleanup inflight requests (TTL)
        self.cleanup_inflight().await;

        // 2) Trigger GetTips
        let _ = self.broadcast(&NetMsg::GetTips { limit: 64 }).await;
    }

    /// Nettoie les requêtes inflight expirées.
    /// Cela permet de relancer des demandes si un pair n'a pas répondu.
    async fn cleanup_inflight(&self) {
        let mut inflight = self.inflight_fetch.lock().await;
        // Keep only requests younger than INFLIGHT_TTL_MS
        inflight.retain(|_, start_time| {
            start_time.elapsed().as_millis() < crate::limits::INFLIGHT_TTL_MS
        });
    }

    /// Connexion sortante : diale un serveur distant et le traite comme un pair entrant.
    pub async fn connect(self: &Arc<Self>, addr: &str) -> anyhow::Result<()> {
        self.dial(addr).await
    }

    /// Démarre le pipeline I/O pour une connexion sortante.
    async fn dial(self: &Arc<Self>, addr: &str) -> anyhow::Result<()> {
        let stream = TcpStream::connect(addr).await?;
        let sa = stream.peer_addr()?; // adresse du remote
        self.handle_new_peer(stream, sa).await?;

        // prépare le waiter pour le Pong
        let (tx, rx) = oneshot::channel::<()>();
        self.pong_waiters.insert(sa, tx);

        // envoie Ping
        let _ = self
            .unicast(
                &sa,
                NetMsg::Hello {
                    proto: 1,
                    node_id: self.node_id.clone(),
                    nonce: random::<u64>(),
                    ping_ms: PING_EVERY_MS,
                },
            )
            .await;

        // attend Pong (best effort, 500 ms)
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), rx).await;

        // 👉 Demande les tips même si on n’a pas vu de HelloAck (robustesse)
        let _ = self.unicast(&sa, NetMsg::GetTips { limit: 64 }).await;

        Ok(())
    }

    /// Traite un lot de blocks (ou un seul) avec gestion récursive des orphelins.
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

            let was_inflight = {
                let mut inflight = self.inflight_fetch.lock().await;
                inflight.remove(&wb.id).is_some()
            };

            /*
            if !was_inflight && self.seen_inv_recently_and_mark(&wb.id).await {
                continue; // déjà vu en gossip récemment et pas demandé explicitement
            }
            */
            eprintln!(
                "[SRV] Processing block {} (net={})",
                wb.id.get(..8).unwrap_or(&wb.id),
                &wb.network_id
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
                    if self.parent_dependency.len() < MAX_PARENT_DEPS {
                        eprintln!(
                            "[SRV] Add dep: parent={} child={}",
                            p.get(..8).unwrap_or(p),
                            wb.id.get(..8).unwrap_or(&wb.id)
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
                if self.orphans.len() >= MAX_ORPHANS {
                    tracing::warn!(
                        "Orphan cache full ({} entries), dropping block {}",
                        self.orphans.len(),
                        wb.id.get(..8).unwrap_or(&wb.id)
                    );
                    continue;
                }

                eprintln!(
                    "[SRV] Orphan {} missing {} parents",
                    wb.id.get(..8).unwrap_or(&wb.id),
                    missing_to_fetch.len()
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
                    let mut inflight = self.inflight_fetch.lock().await;
                    if inflight.len() < crate::limits::MAX_INFLIGHT_GETBLOCK
                        || inflight.contains_key(&pid)
                    {
                        inflight.insert(pid.clone(), Instant::now());
                        drop(inflight);
                        let _ = self.unicast(&sa, NetMsg::GetBlock { id: pid }).await;
                    }
                }
                continue;
            }

            // ====== BENCHMARK: Timer persist ======
            let persist_start = tokio::time::Instant::now();
            // =======================================

            println!(
                "[SRV] Calling persist_block for {} (net={})",
                wb.id.get(..8).unwrap_or(&wb.id),
                &wb.network_id
            );
            let result = block_adapter.persist_block(&wb).await;
            println!("[SRV] persist_block returned: {:?}", result);

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
                        eprintln!(
                            "[SRV] Unblocking parent={} -> {} children",
                            wb.id.get(..8).unwrap_or(&wb.id),
                            children.len()
                        );
                        for child_id in children {
                            if let Some((_, child_wb)) = self.orphans.remove(&child_id) {
                                eprintln!(
                                    "[SRV] Queueing unblocked child {}",
                                    child_id.get(..8).unwrap_or(&child_id)
                                );
                                process_queue.push_back(child_wb);
                            } else {
                                eprintln!(
                                    "[SRV] Orphan {} missing from map!",
                                    child_id.get(..8).unwrap_or(&child_id)
                                );
                            }
                        }
                    }
                }
                Ok(PutResult::AlreadyExists) => {
                    eprintln!(
                        "[SRV] Block {} already exists, skipping",
                        wb.id.get(..8).unwrap_or(&wb.id)
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
                    eprintln!("[SRV] {sa} persist REJECT id={} reason={}", wb.id, reason);

                    // Extract missing parent ID from structured rejection messages
                    let missing_parent_id = extract_missing_parent_id(&reason);
                    if let Some(pid_clean) = missing_parent_id {
                        eprintln!("[SRV] {sa} -> GetBlock(missing parent={})", pid_clean);

                        // Save orphan & dep (bounded)
                        if self.orphans.len() < MAX_ORPHANS {
                            self.orphans.insert(wb.id.clone(), wb.clone());
                            if self.parent_dependency.len() < MAX_PARENT_DEPS {
                                self.parent_dependency
                                    .entry(pid_clean.clone())
                                    .or_default()
                                    .push(wb.id.clone());
                            }
                        }

                        // Request parent
                        let mut inflight = self.inflight_fetch.lock().await;
                        if inflight.len() < crate::limits::MAX_INFLIGHT_GETBLOCK
                            || inflight.contains_key(&pid_clean)
                        {
                            inflight.insert(pid_clean.clone(), Instant::now());
                            let _ = self.broadcast(&NetMsg::GetBlock { id: pid_clean }).await;
                        }
                    } else {
                        crate::metrics::BLOCKS_REJECTED
                            .with_label_values(&["main"])
                            .inc();
                    }
                }
                Err(e) => {
                    eprintln!("[SRV] persist ERR id={} err={e}", wb.id);
                }
            }
        }
    }

    async fn mark_inv_seen(&self, id: &str) {
        let mut cache = self.seen_invs.lock().await;
        cache.put(id.to_string(), Instant::now());
        while cache.len() > SEEN_CAPACITY {
            cache.pop_lru();
        }
    }
}

/// Extracts a 64-char hex parent ID from a rejection reason string.
/// Handles formats like:
///   - "dag validation failed: parent {hex64} not found"
///   - "parent {hex64} missing"
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
