//! Synchronization, outbound connections, and inflight request management.

use super::Server;
use pms_config::TlsConfig;
use pms_network::messages::NetMsg;
use rand::random;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use crate::limits::PING_EVERY_MS;

impl Server {
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


    pub async fn trigger_sync(&self) {
        // 1) Cleanup inflight requests (TTL)
        self.cleanup_inflight().await;

        // 2) Trigger GetTips
        let _ = self.broadcast(&NetMsg::GetTips { limit: 64 }).await;
    }

    /// Nettoie les requêtes inflight expirées.
    /// Cela permet de relancer des demandes si un pair n'a pas répondu.
    async fn cleanup_inflight(&self) {
        // DashMap::retain is lock-free per shard — no global lock needed
        self.inflight_fetch.retain(|_, start_time| {
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

}
