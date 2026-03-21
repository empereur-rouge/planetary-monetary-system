//! TCP/TLS listener and server lifecycle management.

use super::Server;
use crate::api;
use crate::stats::Stats;
use pms_config::{ServerConfig, TlsConfig};
use pms_storage::rocks_store::store::RocksStore;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::{sleep, Instant};
use tokio_util::sync::CancellationToken;
use pms_network::messages::NetMsg;

impl Server {
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

        // 1) Background sync: orphan retry (2s) + periodic GetTips (5s) + stats
        {
            let stats = stats.clone();
            let srv_for_sync = self.clone();
            tokio::spawn(async move {
                let mut tick: u64 = 0;
                loop {
                    sleep(Duration::from_secs(2)).await;
                    tick += 1;

                    // 1) Log stats (every ~10s)
                    if tick % 5 == 0 {
                        let (ok, dup, err, go, gr, ge) = stats.snapshot();
                        tracing::info!(
                            target="pms_stats",
                            ptr=?Arc::as_ptr(&stats),
                            "📊 stats: persist ok={} dup={} err={} | gossip ok={} reject={} err={}",
                            ok, dup, err, go, gr, ge
                        );
                    }

                    // 2) Retry Missing Parents for Orphans (every 2s)
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
                            for pid in parents_needed {
                                if let Some(ts) = srv_for_sync.inflight_fetch.get(&pid) {
                                    if ts.elapsed().as_millis() < 2000 {
                                        continue;
                                    }
                                }
                                srv_for_sync.inflight_fetch.insert(pid.clone(), Instant::now());
                                to_fetch.push(pid);
                                if to_fetch.len() >= 100 {
                                    break;
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

                    // 3) Active Sync: Broadcast GetTips every ~10s (tick%5 with 2s interval)
                    // Reduced from 200ms/1024 to 10s/64 to avoid network amplification
                    if tick % 5 == 0 {
                        let _ = srv_for_sync
                            .broadcast(&NetMsg::GetTips { limit: 64 })
                            .await;
                    }
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
                        tracing::error!("API server exited cleanly (unexpected)");
                        std::process::exit(1);
                    }
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, "API server fatal error");
                        std::process::exit(1);
                    }
                    Err(join_err) => {
                        tracing::error!(error = ?join_err, "API server task panicked");
                        std::process::exit(1);
                    }
                }
            });
        }

        // 3) Tâches de maintenance RocksDB (flush / compaction / stats)
        let cancel = CancellationToken::new();
        let maint_handle = store.spawn_background_maintenance(
            cancel.clone(),
            Duration::from_secs(21600), // compact toutes les 6h (L0 drain handled by sub-compactions)
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
                tracing::info!(?mode, %bind_addr, "P2P TLS listener starting");
                self.listen_tls(&cfg.bind_addr, tls).await
            } else {
                // 🧪 Dev / Testnet: on est tolérant
                if !cert_exists || !key_exists {
                    tracing::info!(?mode, %bind_addr, "TLS configured but cert/key files missing, falling back to plain TCP");
                    self.listen(&cfg.bind_addr).await
                } else {
                    tracing::info!(?mode, %bind_addr, "P2P TLS listener starting");
                    self.listen_tls(&cfg.bind_addr, tls).await
                }
            }
        } else {
            // Pas de bloc [tls] → P2P en clair
            tracing::info!(%bind_addr, "no TLS configured, P2P listening on plain TCP");
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
            let permit = match this.conn_semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    tracing::warn!(peer = %sa, "P2P connection rejected: max connections ({}) reached", self.max_connections);
                    drop(tcp);
                    continue;
                }
            };

            tokio::spawn(async move {
                match acceptor.accept(tcp).await {
                    Ok(tls_stream) => {
                        if let Err(e) = this.handle_new_peer_tls(tls_stream, sa).await {
                            tracing::error!(peer = %sa, error = %e, "TLS peer handler error");
                        }
                    }
                    Err(e) => tracing::error!(peer = %sa, error = %e, "TLS accept failed"),
                }
                drop(permit); // Release connection slot when peer disconnects
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
            if self.conn_semaphore.available_permits() == 0 {
                tracing::warn!(peer = %sa, "P2P connection rejected: max connections ({}) reached", self.max_connections);
                drop(stream);
                continue;
            }
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
            if self.conn_semaphore.available_permits() == 0 {
                tracing::warn!(peer = %sa, "P2P connection rejected: max connections ({}) reached", self.max_connections);
                drop(stream);
                continue;
            }
            self.handle_new_peer(stream, sa).await?;
        }
    }

}
