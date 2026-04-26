//! Peer connection handling: handshake, message processing loop, read/write tasks.

use super::{PeerState, Server};
use crate::limits::{
    HANDSHAKE_TIMEOUT_MS, MAX_BLOCKS_BATCH, MAX_LINE_BYTES,
    MAX_PARSE_ERRORS, PING_EVERY_MS, RATE_BURST,
    RATE_MSGS_PER_SEC,
};
use crate::rate::TokenBucket;
use pms_network::messages::NetMsg;
use pms_wire::WireBlock;
use rand::random;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;

impl Server {
    pub(super) async fn handle_new_peer(
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
        writer_io: W,
        sa: SocketAddr,
        is_inbound: bool,
    ) -> anyhow::Result<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        // On encapsule le reader dans un BufReader pour faire des `read_line` efficaces.
        let (tx_out, mut rx_out) = mpsc::channel::<Arc<str>>(self.per_peer_queue_cap);

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
            // Wrap in BufWriter to batch multiple messages before flushing.
            // This reduces TLS record overhead from per-message to periodic (5ms).
            let mut writer = tokio::io::BufWriter::new(writer_io);
            let mut flush_interval =
                tokio::time::interval(Duration::from_millis(5));
            flush_interval
                .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                tokio::select! {
                    biased;
                    msg = rx_out.recv() => {
                        match msg {
                            Some(line) => {
                                if writer.write_all(line.as_bytes()).await.is_err() {
                                    break;
                                }
                                if writer.write_all(b"\n").await.is_err() {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    _ = flush_interval.tick() => {
                        if writer.flush().await.is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = writer.shutdown().await;
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
                    tracing::trace!(peer = %sa, ?msg, "recv msg");

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
                                metadata: metadata.map(|b| *b),
                            };
                            this.process_incoming_blocks(vec![wb], sa).await;
                        }
                        NetMsg::Inv { ids } => {
                            tracing::debug!(peer = %sa, count = ids.len(), "recv Inv");
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
                                tracing::trace!(id = %id.get(..8).unwrap_or(&id), "processing Inv ID");
                                if let Some(ts) = this.inflight_fetch.get(&id) {
                                    if ts.elapsed().as_millis() < crate::limits::INFLIGHT_TTL_MS {
                                        continue;
                                    }
                                }
                                if this.inflight_fetch.len() < this.max_inflight_requests {
                                    this.inflight_fetch.insert(id.clone(), Instant::now());
                                    to_fetch.push(id.clone());
                                    tracing::trace!(id = %id, peer = %sa, "requesting block from peer");
                                } else {
                                    break;
                                }
                            }
                            if !to_fetch.is_empty() {
                                // Send the GetBlock(s) request back to the peer
                                // that announced the Inv (`sa`), not via broadcast.
                                //
                                // Pre-fix this used `broadcast()`, which only fans
                                // out to INBOUND peers (see `broadcast.rs:81-87`).
                                // When the local node was an OUTBOUND peer
                                // relative to the announcer (e.g. a follower
                                // dialing a coordinator), the request went
                                // nowhere — so the announced blocks were never
                                // fetched and propagation silently stalled.
                                // Unicast back to `sa` is both more efficient
                                // (single send instead of fan-out) and direction-
                                // independent: the peer that has the data is
                                // exactly the peer that announced it.
                                let msg = if to_fetch.len() == 1 {
                                    NetMsg::GetBlock {
                                        id: to_fetch.into_iter().next().unwrap(),
                                    }
                                } else {
                                    NetMsg::GetBlocks { ids: to_fetch }
                                };
                                let _ = this.unicast(&sa, msg).await;
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
                            tracing::debug!(peer = %sa, count = ids.len(), "recv GetBlocks");
                            let blocks = this.get_blocks_any(&ids).await;
                            if !blocks.is_empty() {
                                tracing::debug!(peer = %sa, count = blocks.len(), "sending blocks");
                                let _ = this.unicast(&sa, NetMsg::Blocks { blocks }).await;
                            } else {
                                tracing::debug!(requested = ids.len(), "GetBlocks returned empty");
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
                            tracing::debug!(count = ids.len(), "serving GetTips");
                            let _ = this.unicast(&sa, NetMsg::Tips { ids }).await;
                        }
                        NetMsg::Tips { ids } => {
                            let mut to_fetch = Vec::new();
                            tracing::debug!(count = ids.len(), "processing Tips");
                            for id in ids {
                                // Check all ledgers
                                if this.have_block_any(&id).await {
                                    continue;
                                }
                                // BUG FIX: Don't check seen_inv for Tips!
                                // Tips are authoritative sync info. If we don't have the block and it's not inflight,
                                // we must fetch it, even if we saw an Inv recently (e.g. failed fetch).
                                // if this.seen_inv_recently_and_mark(&id).await { countinue; }
                                let in_inflight = this.inflight_fetch.contains_key(&id);
                                tracing::trace!(
                                    id = %id.get(..8).unwrap_or(&id),
                                    in_inflight,
                                    "tip not held locally"
                                );

                                if let Some(ts) = this.inflight_fetch.get(&id) {
                                    if ts.elapsed().as_millis() < crate::limits::INFLIGHT_TTL_MS {
                                        continue;
                                    }
                                }
                                if this.inflight_fetch.len() < this.max_inflight_requests {
                                    this.inflight_fetch.insert(id.clone(), Instant::now());
                                    to_fetch.push(id);
                                }
                            }
                            if !to_fetch.is_empty() {
                                tracing::debug!(count = to_fetch.len(), "sending GetBlocks for missing tips");
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

}
