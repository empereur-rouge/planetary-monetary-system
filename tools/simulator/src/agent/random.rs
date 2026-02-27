use crate::agent::{Agent, AgentContext};
use crate::comms::types::AgentMessage;
use crate::config::AgentGameConfig;
use crate::error::SimResult;
use crate::metrics::MetricEvent;
use crate::types::{SendSimpleRequest, WalletInfo};
use rand::Rng;
use tokio::sync::mpsc;

/// Seuil PMS en-dessous duquel l'agent refuel via faucet
const LOW_BALANCE_THRESHOLD: f64 = 10.0;
/// Amount to faucet during refuel
const REFUEL_AMOUNT: &str = "50.00";
/// Minimum EDN balance to attempt a send
const EDN_SEND_THRESHOLD: f64 = 0.000_000_01;

/// Agent that sends random amounts to random peers — no AI needed.
/// Auto-refuels via faucet when balance drops below threshold.
///
/// Game loop (when game engine is active, every tick):
///   1. Has cubes → batch burn all (earn EDN)
///   2. Has EDN, no cubes → send EDN to random peer
///   3. No cubes, no EDN → re-mint cubes
pub struct RandomAgent {
    name: String,
    wallet: WalletInfo,
    inbox: mpsc::UnboundedReceiver<AgentMessage>,
    cached_balance: f64,
    tick_count: u32,
    min_amount: f64,
    max_amount: f64,
    send_probability: f64,
    /// Cube token IDs owned by this agent
    cube_ids: Vec<String>,
    /// Locally tracked EDN balance (earned from burns, decreased by sends)
    edn_balance: f64,
    /// Per-agent game configuration
    game_config: Option<AgentGameConfig>,
}

impl RandomAgent {
    pub fn new(
        name: String,
        wallet: WalletInfo,
        inbox: mpsc::UnboundedReceiver<AgentMessage>,
        min_amount: f64,
        max_amount: f64,
        send_probability: f64,
        game_config: Option<AgentGameConfig>,
    ) -> Self {
        Self {
            name,
            wallet,
            inbox,
            cached_balance: 0.0,
            tick_count: 0,
            min_amount,
            max_amount,
            send_probability,
            cube_ids: Vec::new(),
            edn_balance: 0.0,
            game_config,
        }
    }

    /// Set initial cube IDs (called after funder mints cubes)
    pub fn set_cube_ids(&mut self, ids: Vec<String>) {
        self.cube_ids = ids;
    }

    fn drain_inbox(&mut self) {
        while self.inbox.try_recv().is_ok() {}
    }

    /// Is the game loop enabled for this agent?
    fn game_enabled(&self) -> bool {
        self.game_config.as_ref().map_or(false, |gc| gc.enabled)
    }

    /// Refuel PMS via faucet (main ledger)
    async fn refuel(&self, ctx: &AgentContext) -> SimResult<()> {
        let addr = &self.wallet.address;

        let faucet_resp = ctx.client.faucet(addr, REFUEL_AMOUNT).await?;
        let block_id = faucet_resp
            .data
            .block_id
            .unwrap_or_default();
        tracing::info!(
            "[{}] Refuel: faucet {} PMS (block {})",
            self.name,
            REFUEL_AMOUNT,
            &block_id[..16.min(block_id.len())]
        );

        let _ = ctx.metrics_tx.send(MetricEvent::AgentFunded {
            agent_name: self.name.clone(),
            amount: REFUEL_AMOUNT.to_string(),
        });

        Ok(())
    }

    /// Game tick: burn all cubes (batch) → send EDN → re-mint when depleted
    async fn game_tick(&mut self, ctx: &AgentContext) -> SimResult<()> {
        let game_engine = match ctx.game_engine {
            Some(ref ge) => ge,
            None => return Ok(()),
        };

        if !self.game_enabled() {
            return Ok(());
        }

        let gc = self.game_config.as_ref().unwrap();

        if !self.cube_ids.is_empty() {
            // ── Phase 1: BATCH BURN all cubes → earn EDN ──
            let cubes_to_burn: Vec<String> = self.cube_ids.drain(..).collect();
            let count = cubes_to_burn.len();
            let ge = game_engine.read().await;
            match ge
                .burn_cubes_for_edenite(
                    &self.wallet.private_key_b64,
                    &self.wallet.address,
                    cubes_to_burn.clone(),
                )
                .await
            {
                Ok((edn_str, burned)) => {
                    let edn: f64 = edn_str.parse().unwrap_or(0.0);
                    self.edn_balance += edn;
                    tracing::info!(
                        "[{}] Batch burned {} cubes → +{} EDN (balance: {:.10})",
                        self.name,
                        burned,
                        edn_str,
                        self.edn_balance
                    );
                    let _ = ctx.metrics_tx.send(MetricEvent::TransactionSent {
                        agent_name: self.name.clone(),
                        block_id: format!("batch-burn:{}", burned),
                        amount: format!("{} EDN", edn_str),
                        latency: std::time::Duration::from_millis(0),
                    });
                    // Broadcast to web dashboard
                    ctx.comms
                        .broadcast(
                            &self.name,
                            AgentMessage::Info {
                                from: self.name.clone(),
                                data: serde_json::json!({
                                    "action": "batch_burn",
                                    "cubes": burned,
                                    "edn_earned": edn_str,
                                    "edn_balance": format!("{:.10}", self.edn_balance),
                                }),
                            },
                        )
                        .await;
                }
                Err(e) => {
                    tracing::warn!(
                        "[{}] Failed to batch burn {} cubes: {:#}",
                        self.name,
                        count,
                        e
                    );
                    // Put them back for retry
                    self.cube_ids = cubes_to_burn;
                }
            }
        } else if self.edn_balance >= EDN_SEND_THRESHOLD {
            // ── Phase 2: SEND EDN to a random peer ──
            let peers = ctx.peer_registry.read().await;
            let others: Vec<_> = peers.iter().filter(|p| p.name != self.name).collect();
            if others.is_empty() {
                return Ok(());
            }
            let target = {
                let mut rng = rand::rng();
                let idx: usize = rng.random_range(0..others.len());
                others[idx].clone()
            };
            drop(peers);

            // Send edn_send_min_pct..edn_send_max_pct % of current EDN balance
            let min_frac = gc.edn_send_min_pct / 100.0;
            let max_frac = gc.edn_send_max_pct / 100.0;
            let fraction = {
                let mut rng = rand::rng();
                rng.random_range(min_frac..max_frac)
            };
            let send_amount = self.edn_balance * fraction;
            let amount_str = format!("{:.10}", send_amount);

            let ge = game_engine.read().await;
            match ge.send_edenite(&target.address, &amount_str).await {
                Ok(()) => {
                    self.edn_balance -= send_amount;
                    tracing::info!(
                        "[{}] Sent {} EDN → {} (remaining: {:.10})",
                        self.name,
                        amount_str,
                        target.name,
                        self.edn_balance
                    );
                    let _ = ctx.metrics_tx.send(MetricEvent::TransactionSent {
                        agent_name: self.name.clone(),
                        block_id: format!("edn-send:{}", &target.name),
                        amount: format!("{} EDN", amount_str),
                        latency: std::time::Duration::from_millis(0),
                    });
                    // Notify web dashboard
                    ctx.comms
                        .send_to(
                            &target.name,
                            AgentMessage::TxNotification {
                                from: self.name.clone(),
                                to: target.name.clone(),
                                amount: format!("{} EDN", amount_str),
                                block_id: "edn-transfer".to_string(),
                            },
                        )
                        .await;
                }
                Err(e) => {
                    tracing::warn!(
                        "[{}] Failed to send EDN to {}: {:#}",
                        self.name,
                        target.name,
                        e
                    );
                }
            }
        } else {
            // ── Phase 3: RE-MINT cubes (out of cubes + out of EDN) ──
            let cubes_to_mint = gc.cubes_per_remint;
            tracing::info!(
                "[{}] No cubes, EDN {:.10} < threshold → re-minting {} cubes",
                self.name,
                self.edn_balance,
                cubes_to_mint
            );
            let mut ge = game_engine.write().await;
            for i in 0..cubes_to_mint {
                match ge
                    .mint_cube(&self.wallet.address, &self.wallet.x25519_pub_hex)
                    .await
                {
                    Ok(token_id) => {
                        tracing::info!(
                            "[{}] Re-minted cube {}/{} → {}",
                            self.name,
                            i + 1,
                            cubes_to_mint,
                            &token_id[..16]
                        );
                        self.cube_ids.push(token_id);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "[{}] Failed to re-mint cube {}: {:#}",
                            self.name,
                            i + 1,
                            e
                        );
                    }
                }
                // Small delay between mints to avoid contention
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            // Notify dashboard
            if !self.cube_ids.is_empty() {
                ctx.comms
                    .broadcast(
                        &self.name,
                        AgentMessage::Info {
                            from: self.name.clone(),
                            data: serde_json::json!({
                                "action": "remint",
                                "cubes_minted": self.cube_ids.len(),
                            }),
                        },
                    )
                    .await;
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl Agent for RandomAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn wallet(&self) -> &WalletInfo {
        &self.wallet
    }

    async fn tick(&mut self, ctx: &AgentContext) -> SimResult<()> {
        self.drain_inbox();
        self.tick_count += 1;

        // Refresh PMS balance every 5 ticks
        if self.tick_count % 5 == 0 {
            let bal_str = ctx
                .client
                .balance(&self.wallet.address)
                .await
                .unwrap_or_else(|_| "0".to_string());

            self.cached_balance = bal_str.parse::<f64>().unwrap_or(0.0);

            let _ = ctx.metrics_tx.send(MetricEvent::BalanceUpdate {
                agent_name: self.name.clone(),
                balance: bal_str,
            });
        }

        // Auto-refuel PMS quand le solde est trop bas
        if self.cached_balance < LOW_BALANCE_THRESHOLD {
            tracing::info!(
                "[{}] Balance {:.2} PMS < {:.2}, refueling...",
                self.name,
                self.cached_balance,
                LOW_BALANCE_THRESHOLD
            );
            match self.refuel(ctx).await {
                Ok(()) => {
                    let bal_str = ctx
                        .client
                        .balance(&self.wallet.address)
                        .await
                        .unwrap_or_else(|_| "0".to_string());
                    self.cached_balance = bal_str.parse::<f64>().unwrap_or(0.0);
                }
                Err(e) => {
                    tracing::warn!("[{}] Refuel failed: {:#}", self.name, e);
                    let _ = ctx.metrics_tx.send(MetricEvent::AgentError {
                        agent_name: self.name.clone(),
                        error: format!("refuel failed: {:#}", e),
                    });
                    return Ok(());
                }
            }
        }

        // Game tick every tick (frequency controlled by interval_ms)
        self.game_tick(ctx).await?;

        // Decide whether to send PMS this tick
        let (should_send, peer_idx, amount_str) = {
            let mut rng = rand::rng();
            let send = rng.random::<f64>() <= self.send_probability;
            let idx: usize = rng.random_range(0..usize::MAX);
            let amount = rng.random_range(self.min_amount..=self.max_amount);
            (send, idx, format!("{:.2}", amount))
        };

        if !should_send {
            return Ok(());
        }

        // Pick a random peer (not self)
        let peers = ctx.peer_registry.read().await;
        let others: Vec<_> = peers.iter().filter(|p| p.name != self.name).collect();
        if others.is_empty() {
            return Ok(());
        }
        let target = others[peer_idx % others.len()].clone();
        drop(peers);

        // Send PMS
        match ctx
            .client
            .send_simple(&SendSimpleRequest {
                private_key_b64: self.wallet.private_key_b64.clone(),
                to: target.address.clone(),
                amount: amount_str.clone(),
                asset_id: None,
            })
            .await
        {
            Ok(resp) => {
                let block_id = resp.data.block_id.unwrap_or_default();

                ctx.comms
                    .send_to(
                        &target.name,
                        AgentMessage::TxNotification {
                            from: self.name.clone(),
                            to: target.name.clone(),
                            amount: amount_str.clone(),
                            block_id: block_id.clone(),
                        },
                    )
                    .await;

                let _ = ctx.metrics_tx.send(MetricEvent::TransactionSent {
                    agent_name: self.name.clone(),
                    block_id,
                    amount: amount_str,
                    latency: resp.latency,
                });
            }
            Err(e) => {
                let _ = ctx.metrics_tx.send(MetricEvent::AgentError {
                    agent_name: self.name.clone(),
                    error: format!("{:#}", e),
                });
            }
        }

        Ok(())
    }
}
