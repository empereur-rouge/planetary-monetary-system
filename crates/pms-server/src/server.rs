//! Serveur P2P minimal avec diffusion (gossip) de blocs.
//!
//! Objectifs pédagogiques :
//! - Montrer comment structurer un serveur réseau asynchrone avec Tokio
//! - Expliquer pourquoi on a besoin de `Send + Sync + 'static` sur l’adapter
//! - Illustrer un flux lecture/écriture par peer, plus un broadcast simple

use crate::api;
use crate::limits::{
    HANDSHAKE_TIMEOUT_MS, MAX_BLOCKS_BATCH, MAX_INFLIGHT_GETBLOCK, MAX_LINE_BYTES,
    MAX_PARSE_ERRORS, PER_PEER_Q_CAP, PING_EVERY_MS, RATE_BURST, RATE_MSGS_PER_SEC, SEEN_CAPACITY,
};
use crate::rate::TokenBucket;
use crate::stats::Stats;
use dashmap::DashMap;
use lru::LruCache;
use pms_interface::NetDagAdapter;
use pms_network::messages::NetMsg;
use pms_wire::WireBlock;
use rand::random;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::{net::SocketAddr, sync::Arc};
use std::path::Path;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, oneshot};
use tokio::time::{Instant, sleep};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
use pms_config::{NetworkMode, ServerConfig, Settings, TlsConfig};
use pms_storage::rocks_store::store::RocksStore;
use pms_storage::store::PutResult;
use pms_wallet::{SignerBackend, Wallet};

const SEEN_TTL: Duration = Duration::from_secs(60);

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
    node_id: String,                               // ident local
    seen_blocks: Mutex<LruCache<String, Instant>>, // LRU + horodatage pour TTL
    inflight_fetch: Mutex<HashSet<String>>,
    network_id: String,
    protocol_version: u32,
    node_wallet: Arc<Wallet>,
}

#[derive(Debug)]
struct PeerState {
    /// File de sortie vers ce pair (messages que NOUS lui envoyons)
    tx: mpsc::Sender<NetMsg>,
    /// Seau à jetons par pair pour limiter le débit (anti-flood)
    bucket: TokenBucket,
    /// Compteur d’erreurs de parsing JSON successives
    parse_errors: u32,
    /// Octets lus dans la fenêtre courante (indicatif / debug)
    bytes_in_window: u64,
    /// Dernière activité vue (pour timeouts, stats)
    last_seen: Instant,
}

impl Server {
    /// Construit un serveur autour d’un adapter.
    /// On retourne un `Arc<Self>` car on a besoin de cloner le serveur dans les tâches spawnées.
    pub fn new(adapter: Arc<dyn NetDagAdapter>,network_id: impl Into<String>, protocol_version: u32, node_wallet: Arc<Wallet>,) -> Arc<Self> {
        let node_id = node_wallet.encoded_public_key(); // ou hash de la clé, à toi de voir
        
        Arc::new(Self {
            adapter,
            peers: DashMap::new(),
            pong_waiters: DashMap::new(),
            node_id,
            seen_blocks: Mutex::new(LruCache::new(SEEN_CAPACITY.try_into().unwrap())),
            inflight_fetch: Mutex::new(HashSet::new()),
            network_id: network_id.into(),
            protocol_version,
            node_wallet
        })
    }

    pub fn adapter_arc(&self) -> Arc<dyn NetDagAdapter> {
        self.adapter.clone()
    }

    pub fn node_identity_wallet(&self) -> &Wallet {
        &self.node_wallet
    }

    pub async fn run(
        self: Arc<Self>,
        cfg: Arc<ServerConfig>,
        store: Arc<RocksStore>,
    ) -> anyhow::Result<()> {
        let stats = Arc::new(Stats::new());
        let ready = Arc::new(AtomicBool::new(false));

        tracing::info!(target="pms_stats", ptr=?Arc::as_ptr(&stats), "stats_ptr run()");

        // 1) Logger périodique des stats (persist / gossip)
        {
            let stats = stats.clone();
            tokio::spawn(async move {
                loop {
                    sleep(Duration::from_secs(5)).await;
                    let (ok, dup, err, go, gr, ge) = stats.snapshot();
                    tracing::info!(
                        target="pms_stats",
                        ptr=?Arc::as_ptr(&stats),
                        "📊 5s: persist ok={} dup={} err={} | gossip ok={} reject={} err={}",
                        ok, dup, err, go, gr, ge
                    );
                }
            });
        }

        // 2) Démarrage de l’API HTTP (TLS ou non, la logique est dans run_api)
        {
            let srv           = self.clone();
            let addr          = cfg.api_addr.clone();
            let cfg_for_api   = cfg.clone();
            let stats_for_api = stats.clone();
            let ready_for_api = ready.clone();
            let store_for_api = store.clone();

            tokio::spawn(async move {
                if let Err(e) = srv
                    .run_api(addr, cfg_for_api, ready_for_api, stats_for_api, store_for_api)
                    .await
                {
                    eprintln!("[API] error: {e}");
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
            let key_exists  = Path::new(&tls.key_pem).exists();

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
                        mode,
                        bind_addr
                    );
                    self.listen(&cfg.bind_addr).await
                } else {
                    eprintln!(
                        "[P2P] mode {:?} avec TLS → listen_tls({})",
                        mode,
                        bind_addr
                    );
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
        static READY: once_cell::sync::Lazy<Arc<AtomicBool>> =
            once_cell::sync::Lazy::new(|| Arc::new(AtomicBool::new(true)));
        api::serve_api(&addr, self, cfg, ready, stats, store).await?;
        Ok(())
    }


    pub async fn listen_tls(
        self: Arc<Self>,
        bind: &str,
        tls: TlsConfig,
    ) -> anyhow::Result<()> {
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
        let (r, w) = tokio::io::split(stream);
        self.handle_new_peer_from_io(r, w, sa).await
    }

    pub async fn handle_new_peer_tls(
        self: &Arc<Self>,
        tls_stream: tokio_rustls::server::TlsStream<TcpStream>,
        sa: SocketAddr,
    ) -> anyhow::Result<()> {
        let (r, w) = tokio::io::split(tls_stream);
        self.handle_new_peer_from_io(r, w, sa).await
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
    ) -> anyhow::Result<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        use tokio::io::BufReader;

        // On encapsule le reader dans un BufReader pour faire des `read_line` efficaces.
        let mut reader = BufReader::with_capacity(MAX_LINE_BYTES, reader_io);

        // Canal de sortie pour ce peer:
        // - n’importe quelle partie du serveur peut envoyer un `NetMsg` à ce peer via `PeerState.tx`.
        // - la tâche d’écriture ci-dessous consomme ce canal et pousse vers le socket.
        let (tx_out, mut rx_out) = mpsc::channel::<NetMsg>(PER_PEER_Q_CAP);

        // On enregistre l’état de ce peer dans `self.peers`.
        // - `bucket` = token bucket pour limiter le nombre de PONGs émis (anti-flood).
        // - `parse_errors` = compteur d’erreurs de JSON, pour kick un peer qui spamme du garbage.
        // - `bytes_in_window` + `last_seen` = base pour d’autres limites de débit si tu veux.
        self.peers.insert(
            sa,
            PeerState {
                tx: tx_out,
                bucket: TokenBucket::new(RATE_MSGS_PER_SEC, RATE_BURST),
                parse_errors: 0,
                bytes_in_window: 0,
                last_seen: Instant::now(),
            },
        );

        // On envoie un Hello sortant dès qu’on accepte le peer.
        // C’est la première étape du handshake.
        let hello = NetMsg::Hello {
            proto: 1,
            node_id: self.node_id.clone(),
            nonce: random::<u64>(),
            ping_ms: PING_EVERY_MS,
        };
        let _ = self.unicast(&sa, hello).await;

        // ========================
        // 1) TÂCHE ÉCRITURE
        // ========================
        //
        // Lit les NetMsg depuis `rx_out` (canal interne) et les écrit sur le socket en JSONL:
        //   {json}\n{json}\n...
        //
        // Quand le canal est fermé (plus de sender), on ferme proprement le writer et on retire le peer.
        let this = Arc::clone(self);
        tokio::spawn(async move {
            while let Some(m) = rx_out.recv().await {
                if let Ok(s) = serde_json::to_string(&m) {
                    let _ = writer_io.write_all(s.as_bytes()).await;
                    let _ = writer_io.write_all(b"\n").await;
                }
            }
            let _ = writer_io.shutdown().await;
            this.peers.remove(&sa);
        });

        // ========================
        // 2) TÂCHE LECTURE
        // ========================
        //
        // Cette tâche lit ligne par ligne, parse un `NetMsg`, vérifie le handshake,
        // applique l’anti-abuse, et exécute la logique P2P / DAG pour chaque message.
        let mut handshaked = false;
        let handshake_deadline = Instant::now() + Duration::from_millis(HANDSHAKE_TIMEOUT_MS);

        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut line = String::new();

            // Boucle principale de lecture JSONL.
            while reader
                .read_line(&mut line)
                .await
                .ok()
                .filter(|&n| n > 0)
                .is_some()
            {
                // 1) Timeout handshake:
                //    Si on n’a pas encore handshaké et que le délai est dépassé, on coupe.
                if !handshaked && Instant::now() > handshake_deadline {
                    eprintln!("[SRV] {sa} handshake timeout");
                    this.peers.remove(&sa);
                    break;
                }

                // 2) Taille max de la ligne (borne dure anti-abuse).
                if line.len() > MAX_LINE_BYTES {
                    this.peers.remove(&sa);
                    break; // entrée trop longue -> kick
                }

                // 3) Parse JSON en `NetMsg`.
                let parsed = serde_json::from_str::<NetMsg>(&line);
                if parsed.is_err() {
                    // Incrémente un compteur d’erreurs de parsing pour ce peer.
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

                // ==================================
                // 3.bis) GESTION HANDSHAKE
                // ==================================
                //
                // Tant que `handshaked == false`, on n’accepte que:
                //   - NetMsg::Hello
                //   - NetMsg::HelloAck
                // Tout autre message avant handshake est considéré comme suspect → on coupe.
                if !handshaked {
                    match msg {
                        NetMsg::Hello { proto, .. } => {
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
                            line.clear();
                            continue;
                        }
                        NetMsg::HelloAck { ok, .. } => {
                            if !ok {
                                this.peers.remove(&sa);
                                break;
                            }
                            handshaked = true; // on considère que nous avons déjà envoyé Hello côté dialer
                            line.clear();
                            continue;
                        }
                        _ => {
                            eprintln!("[SRV] {sa} msg avant handshake");
                            this.peers.remove(&sa);
                            break;
                        }
                    }
                }

                // ==================================
                // 4) TRAITEMENT DES MESSAGES P2P
                // ==================================
                match msg {
                    // --------- PING / PONG ----------
                    NetMsg::Ping => {
                        // On ne limite pas la réception de Ping, mais on limite les Pongs émis.
                        let mut allow = false;
                        if let Some(mut pe) = this.peers.get_mut(&sa) {
                            allow = pe.bucket.take(1); // token bucket: autorise X PONG/s
                        }
                        if allow {
                            let _ = this.unicast(&sa, NetMsg::Pong).await;
                        } else {
                            // drop silencieux: le test s'attend à moins de Pongs que de Pings.
                        }
                    }
                    NetMsg::Pong => {
                        // Un Pong reçu débloque éventuellement un waiter (ping synchrone / healthcheck).
                        if let Some((_, waiter)) = this.pong_waiters.remove(&sa) {
                            let _ = waiter.send(());
                        }
                    }

                    // --------- BLOC SIMPLE (chemin P2P direct) ----------
                    NetMsg::Block {
                        id,
                        parents,
                        payload_json,
                        nonce,
                        network_id,
                        protocol_version,
                        signer_pk_hex,
                        signature_hex,
                    } => {
                        // 1) Anti-rejeu réseau (cache LRU): si on a déjà vu récemment cet id, on ignore.
                        if this.seen_block_recently_and_mark(&id).await {
                            line.clear();
                            continue;
                        }
                        // 2) Si on a déjà ce bloc en store/DAG, inutile d’aller plus loin.
                        if this.adapter.have_block(&id).await {
                            line.clear();
                            continue;
                        }

                        // 3) On reconstruit un `WireBlock` complet, avec les mêmes champs
                        //    que ceux transportés dans NetMsg::Block (incluant réseau + signature).
                        let wb = WireBlock {
                            id,
                            parents,
                            payload_json,
                            nonce,
                            network_id,
                            protocol_version,
                            signer_pk_hex,
                            signature_hex,
                        };

                        // 4) Persist via l’adapter:
                        //    - vérifie `network_id` / `protocol_version` (doivent matcher local)
                        //    - vérifie la signature (verify_block_signature)
                        //    - applique des checks light structurels
                        //    - persiste en Rocks + met à jour DAG RAM + finalité
                        match this.adapter.persist_block(&wb).await {
                            Ok(PutResult::Inserted) => {
                                crate::metrics::BLOCKS_PERSISTED.inc();
                                this.mark_block_seen(&wb.id).await; // évite l’écho
                                // Re-gossip sous forme de `Inv` pour propagation légère aux autres peers.
                                let _ = this
                                    .broadcast_except(
                                        &sa,
                                        &NetMsg::Inv {
                                            ids: vec![wb.id.clone()],
                                        },
                                    )
                                    .await;
                                crate::metrics::BLOCKS_BROADCAST.inc();
                            }
                            Ok(PutResult::AlreadyExists) => { /* déjà présent, rien à faire */ }
                            Ok(PutResult::Rejected(reason)) => {
                                eprintln!("[SRV] {sa} persist REJECT id={} reason={}", wb.id, reason);
                                crate::metrics::BLOCKS_REJECTED.inc();
                            }
                            Err(e) => {
                                eprintln!("[SRV] persist ERR id={} err={e}", wb.id);
                            }
                        }
                    }

                    // --------- HELLO / HELLOACK post-handshake (gestion GetTips côté dialer/accept) ----------
                    NetMsg::Hello { proto, .. } => {
                        eprintln!("[SRV] {sa} -> GetTips");
                        if !handshaked {
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

                            // Dès que le handshake est OK, on demande les tips pour se resynchroniser.
                            let _ = this.unicast(&sa, NetMsg::GetTips { limit: 64 }).await;
                            line.clear();
                            continue;
                        }
                        // Si on est déjà handshaked, on ignore un Hello tardif.
                    }
                    NetMsg::HelloAck { ok, .. } => {
                        eprintln!("[SRV] {sa} <- HelloAck(ok={ok})");
                        eprintln!("[SRV] {sa} -> GetTips");
                        if !handshaked {
                            if !ok {
                                this.peers.remove(&sa);
                                break;
                            }
                            handshaked = true;

                            // Côté dialer aussi, on déclenche un GetTips après ack.
                            let _ = this.unicast(&sa, NetMsg::GetTips { limit: 64 }).await;
                            line.clear();
                            continue;
                        }
                        // Ack tardif: ignoré
                    }

                    // --------- SYNC / RATTRAPAGE ----------
                    // 1) Le pair nous demande nos tips
                    NetMsg::GetTips { limit } => {
                        eprintln!("[SRV] {sa} <- GetTips({limit})");
                        // Idéalement: on prend les tips depuis le store (plus robuste).
                        let ids = this
                            .adapter
                            .top_tips(limit)
                            .await
                            .unwrap_or_else(|_| Vec::new());
                        let _ = this.unicast(&sa, NetMsg::Tips { ids }).await;
                    }

                    // 2) Le pair nous envoie ses tips
                    NetMsg::Tips { ids } => {
                        eprintln!("[SRV] {sa} <- Tips(ids={})", ids.len());
                        // Pour chaque tip, si on ne l’a pas, on envoie un GetBlock ciblé.
                        for id in ids {
                            if this.adapter.have_block(&id).await {
                                continue;
                            }
                            if this.seen_block_recently_and_mark(&id).await {
                                continue;
                            }
                            let mut inflight = this.inflight_fetch.lock().await;
                            if inflight.len() >= MAX_INFLIGHT_GETBLOCK {
                                break;
                            }
                            if inflight.insert(id.clone()) {
                                eprintln!("[SRV] {sa} -> GetBlock({id})");
                                let _ = this.unicast(&sa, NetMsg::GetBlock { id }).await;
                            }
                        }
                    }

                    // 3) Le pair nous annonce des ids (Inv = “j’ai ces blocs”)
                    NetMsg::Inv { ids } => {
                        for id in ids {
                            if this.adapter.have_block(&id).await {
                                continue;
                            }
                            if this.seen_block_recently_and_mark(&id).await {
                                continue;
                            }
                            let mut inflight = this.inflight_fetch.lock().await;
                            if inflight.len() >= MAX_INFLIGHT_GETBLOCK {
                                break;
                            }
                            if inflight.insert(id.clone()) {
                                let _ = this.unicast(&sa, NetMsg::GetBlock { id }).await;
                            }
                        }
                    }

                    // 4) Le pair nous demande un bloc précis
                    NetMsg::GetBlock { id } => {
                        eprintln!("[SRV] {sa} <- GetBlock({id})");
                        // On expose `adapter.get_block` qui renvoie un `WireBlock` complet s’il existe.
                        match this.adapter.get_block(&id).await {
                            Ok(Some(wb)) => {
                                eprintln!("[SRV] {sa} -> Blocks(1) id={}", wb.id);
                                // On répond toujours aux requêtes directes, sans regarder le cache anti-rejeu.
                                let _ =
                                    this.unicast(&sa, NetMsg::Blocks { blocks: vec![wb] }).await;
                            }
                            Ok(None) => {
                                eprintln!("[SRV] {sa} get_block miss {id}");
                                let mut inflight = this.inflight_fetch.lock().await;
                                inflight.remove(&id);
                            }
                            Err(e) => {
                                eprintln!("[SRV] get_block({id}) error: {e}");
                                let mut inflight = this.inflight_fetch.lock().await;
                                inflight.remove(&id);
                            }
                        }
                    }

                    // 5) Le pair nous envoie un lot de blocs complets
                    NetMsg::Blocks { mut blocks } => {
                        eprintln!("[SRV] {sa} <- Blocks(n={})", blocks.len());

                        // Bouton panique: on tronque les batchs trop gros.
                        if blocks.len() > MAX_BLOCKS_BATCH {
                            blocks.truncate(MAX_BLOCKS_BATCH);
                        }

                        for mut wb in blocks {
                            // IMPORTANT:
                            // - On force `network_id` et `protocol_version` à ceux du nœud local.
                            //   Hypothèse: ce canal ne sert qu’au sync entre nœuds du même réseau.
                            //   Si un nœud malveillant essaie de jouer avec ces champs, on ne lui fait pas confiance.
                            wb.network_id = this.network_id.clone();
                            wb.protocol_version = this.protocol_version as u16;

                            // Était-il demandé explicitement ?
                            let was_inflight = {
                                let mut inflight = this.inflight_fetch.lock().await;
                                inflight.remove(&wb.id)
                            };

                            // Si bloc non demandé et déjà vu récemment via `Inv`, on l’ignore.
                            if !was_inflight && this.seen_block_recently_and_mark(&wb.id).await {
                                continue;
                            }
                            if this.adapter.have_block(&wb.id).await {
                                continue;
                            }

                            // Pipeline complet d’insert:
                            match this.adapter.persist_block(&wb).await {
                                Ok(PutResult::Inserted) => {
                                    eprintln!("[SRV] {sa} persist OK id={}", wb.id);

                                    // On informe les autres pairs via un `Inv` léger.
                                    let _ = this
                                        .broadcast_except(
                                            &sa,
                                            &NetMsg::Inv {
                                                ids: vec![wb.id.clone()],
                                            },
                                        )
                                        .await;

                                    // On demande les parents manquants (GetBlock) pour rattraper le DAG.
                                    for p in &wb.parents {
                                        if this.adapter.have_block(p).await {
                                            continue;
                                        }
                                        let mut inflight = this.inflight_fetch.lock().await;
                                        if inflight.len() >= MAX_INFLIGHT_GETBLOCK {
                                            break;
                                        }
                                        if inflight.insert(p.clone()) {
                                            eprintln!("[SRV] {sa} -> GetBlock(parent={})", p);
                                            let _ = this
                                                .unicast(&sa, NetMsg::GetBlock { id: p.clone() })
                                                .await;
                                        }
                                    }
                                }
                                Ok(PutResult::AlreadyExists) => {
                                    eprintln!("[SRV] {sa} persist SKIP (dup) id={}", wb.id)
                                }
                                Ok(PutResult::Rejected(reason)) => {
                                    eprintln!("[SRV] {sa} persist REJECT id={} reason={}", wb.id, reason);
                                    crate::metrics::BLOCKS_REJECTED.inc();
                                    // Ici, tu pourrais décider de couper la connexion si trop de blocs invalides.
                                }
                                Err(e) => {
                                    eprintln!("[SRV] persist ERR id={} err={e}", wb.id);
                                }
                            }
                        }
                    }
                }

                // On réinitialise le buffer pour la prochaine ligne JSONL.
                line.clear();
            }

            // Nettoyage best effort à la fin de la boucle (déconnexion, EOF, etc.).
            // `peers.remove` est déjà fait côté writer, donc on ne double-pas ici.
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
            self.mark_block_seen(id).await;
        }
        for pe in self.peers.iter() {
            let _ = pe.tx.send(msg.clone()).await;
        }
        Ok(())
    }

    /// Variante : broadcast sauf `skip`.
    pub async fn broadcast_except(&self, skip: &SocketAddr, msg: &NetMsg) -> anyhow::Result<()> {
        if let NetMsg::Block { id, .. } = msg {
            self.mark_block_seen(id).await;
        }
        for pe in self.peers.iter() {
            if pe.key() == skip {
                continue;
            }
            let _ = pe.tx.send(msg.clone()).await;
        }
        Ok(())
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

    async fn seen_block_recently_and_mark(&self, id: &str) -> bool {
        let now = Instant::now();
        let mut cache = self.seen_blocks.lock().await;

        if let Some(ts) = cache.get(id) {
            if now.duration_since(*ts) < SEEN_TTL {
                return true; // déjà vu récemment
            }
        }
        cache.put(id.to_string(), now);
        while cache.len() > SEEN_CAPACITY {
            cache.pop_lru();
        }
        false
    }

    async fn mark_block_seen(&self, id: &str) {
        let mut cache = self.seen_blocks.lock().await;
        cache.put(id.to_string(), Instant::now());
        while cache.len() > SEEN_CAPACITY {
            cache.pop_lru();
        }
    }
}
