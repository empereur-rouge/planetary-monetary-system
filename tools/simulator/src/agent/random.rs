use crate::agent::{Agent, AgentContext};
use crate::client::DagClient;
use crate::comms::types::AgentMessage;
use crate::config::AgentGameConfig;
use crate::error::SimResult;
use crate::game::GameEngine;
use crate::metrics::MetricEvent;
use crate::sim_metrics::{
    group_of, SIM_BURN_BATCHES, SIM_CUBES_BURNED, SIM_CUBES_MINTED,
    SIM_TX_FAILED, SIM_TX_SENT,
};
use crate::types::{SendSimpleRequest, WalletInfo};
use rand::Rng;
use tokio::sync::mpsc;

/// Classify a `SimError` into the coarse `reason` bucket exposed in
/// `pms_simulator_tx_failed_total{reason=...}`.
fn classify_error(e: &crate::error::SimError) -> &'static str {
    let s = format!("{:#}", e);
    let s_lower = s.to_lowercase();
    if s_lower.contains("insufficient") || s_lower.contains("balance") {
        "insufficient_funds"
    } else if s_lower.contains("4") && (s_lower.contains("400") || s_lower.contains("422") || s_lower.contains("429") || s_lower.contains("401") || s_lower.contains("403"))
    {
        "http_4xx"
    } else if s_lower.contains("5") && (s_lower.contains("500") || s_lower.contains("502") || s_lower.contains("503"))
    {
        "http_5xx"
    } else if s_lower.contains("network") || s_lower.contains("connect") || s_lower.contains("timeout") || s_lower.contains("dns")
    {
        "network_error"
    } else if s_lower.contains("decode") || s_lower.contains("json") || s_lower.contains("parse") {
        "decode_error"
    } else {
        "other"
    }
}

/// Seuil PMS en-dessous duquel l'agent demande un refuel au coordinator
const LOW_BALANCE_THRESHOLD: f64 = 10.0;
/// Amount the coordinator sends during refuel
const REFUEL_AMOUNT: &str = "50.00";
/// Minimum EDN balance to attempt a send
const EDN_SEND_THRESHOLD: f64 = 0.000_000_01;

/// Agent that sends random amounts to random peers — no AI needed.
/// Auto-refuels via coordinator wallet when balance drops below threshold.
///
/// Game loop (when game engine is active, every tick):
///   1. Has cubes → batch burn all (earn EDN via smart contract)
///   2. Has EDN, no cubes → send EDN to random peer
///   3. No cubes, no EDN → re-mint cubes
pub struct RandomAgent {
    name: String,
    wallet: WalletInfo,
    inbox: mpsc::Receiver<AgentMessage>,
    cached_balance: f64,
    tick_count: u32,
    min_amount: f64,
    max_amount: f64,
    send_probability: f64,
    /// Number of PMS transactions to send per tick (default: 1).
    /// Increase to multiply PMS throughput without adding more agents.
    sends_per_tick: u32,
    /// Cube token IDs owned by this agent
    cube_ids: Vec<String>,
    /// Per-agent game configuration
    game_config: Option<AgentGameConfig>,
    /// Ticks remaining before reminting is allowed (gives fee_distribution time to deliver EDN)
    burn_cooldown: u32,
    /// Cached game client + edenite_asset_id (cloned once, never changes after setup)
    game_client_cache: Option<(DagClient, String)>,
}

impl RandomAgent {
    pub fn new(
        name: String,
        wallet: WalletInfo,
        inbox: mpsc::Receiver<AgentMessage>,
        min_amount: f64,
        max_amount: f64,
        send_probability: f64,
        sends_per_tick: u32,
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
            sends_per_tick: sends_per_tick.max(1),
            cube_ids: Vec::new(),
            game_config,
            burn_cooldown: 0,
            game_client_cache: None,
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

    /// Refuel PMS via coordinator wallet (coordinator sends to this agent)
    async fn refuel(&self, ctx: &AgentContext) -> SimResult<()> {
        let coord = ctx.coordinator_wallet.as_ref().ok_or_else(|| {
            crate::error::SimError::Other(anyhow::anyhow!("No coordinator wallet configured for refuel"))
        })?;

        let resp = ctx
            .client
            .send_simple(&SendSimpleRequest {
                private_key_b64: coord.private_key_b64.clone(),
                to: self.wallet.address.clone(),
                amount: REFUEL_AMOUNT.to_string(),
                asset_id: None,
            })
            .await?;
        let block_id = resp.data.block_id.unwrap_or_default();
        tracing::info!(
            "[{}] Refuel: coordinator → {} PMS (block {})",
            self.name,
            REFUEL_AMOUNT,
            &block_id[..16.min(block_id.len())]
        );

        let _ = ctx.metrics_tx.try_send(MetricEvent::AgentFunded {
            agent_name: self.name.clone(),
            amount: REFUEL_AMOUNT.to_string(),
        });

        Ok(())
    }

    /// Ensure game client cache is populated (cloned once, never changes after setup).
    /// Returns a reference to the cached `(DagClient, edenite_asset_id)`.
    async fn ensure_game_cache(
        &mut self,
        game_engine: &tokio::sync::RwLock<GameEngine>,
    ) -> &(DagClient, String) {
        if self.game_client_cache.is_none() {
            let ge = game_engine.read().await;
            self.game_client_cache = Some((
                ge.game_client().clone(),
                ge.edenite_asset_id().to_string(),
            ));
        }
        self.game_client_cache.as_ref().unwrap()
    }

    /// Game tick: burn all cubes (batch) → send EDN → re-mint when depleted.
    ///
    /// Lock-free pattern: write locks are held ONLY for quick registry operations
    /// (HashMap insert/remove), never during HTTP calls. This eliminates the
    /// serialization bottleneck that was capping Eden TPS at ~60.
    ///
    /// Burn cooldown: after burning, waits `burn_cooldown_ticks` ticks before
    /// reminting, giving `fee_distribution` time to deliver EDN UTXOs so
    /// Phase 2 (send EDN) can fire.
    /// Apply optional jitter on the burn cooldown so the fleet doesn't
    /// burn in lockstep. Returns the cooldown to set: `base × (1 + U(-j, +j))`,
    /// rounded to a tick. Falls back to base when jitter is zero or
    /// non-finite. Pinned to at least 1 tick so EDN delivery has a
    /// chance to land before the next mining wave.
    fn jittered_cooldown(base: u32, jitter_pct: f64) -> u32 {
        if !jitter_pct.is_finite() || jitter_pct <= 0.0 {
            return base;
        }
        let mut rng = rand::rng();
        let factor: f64 = 1.0 + rng.random_range(-jitter_pct..jitter_pct);
        let scaled = (base as f64 * factor.max(0.0)).round() as i64;
        scaled.max(1) as u32
    }

    async fn game_tick(&mut self, ctx: &AgentContext) -> SimResult<()> {
        if !self.game_enabled() {
            return Ok(());
        }

        let gc = self.game_config.as_ref().unwrap().clone();

        // Resolve which game ledger this agent plays on. With
        // recommendation #2 (multi-ledger games), `game_index` lets an
        // agent group target a specific entry in `[[simulation.games]]`
        // — different groups pointing at different indices spread the
        // population across N independent game ledgers, exercising
        // every one in parallel.
        let game_engine = match ctx.game_engine_for(gc.game_index) {
            Some(ge) => ge,
            None => return Ok(()),
        };

        // Decrement burn cooldown each tick
        if self.burn_cooldown > 0 {
            self.burn_cooldown -= 1;
        }

        // ── PROGRESSIVE MODE GUARD ──
        //
        // When `target_cubes` is set, the agent mines incrementally
        // toward that target instead of bulk-batching. This gives a
        // continuous N cubes/min stream (configurable via
        // `mint_per_tick`) that matches a real EDN-clicker player —
        // mine, mine, mine, … (≈ 1 hour) … burn, repeat.
        //
        // Burn fires only when inventory ≥ target. While accumulating
        // (cubes < target), the legacy Phase 1 (always-burn) is
        // suppressed; instead Phase 0 (below) mints `mint_per_tick`
        // more cubes per tick. Phase 2 (EDN send) and the cooldown
        // logic still apply when cubes==0 post-burn.
        let progressive_target = gc.target_cubes;
        let should_burn = match progressive_target {
            Some(t) => self.cube_ids.len() >= t.max(1),
            None => !self.cube_ids.is_empty(),
        };

        if should_burn {
            // ── Phase 1: BATCH BURN all cubes → EDN via smart contract ──
            // Lock-free: drain registry under brief write lock, HTTP without lock.
            let cubes_to_burn: Vec<String> = self.cube_ids.drain(..).collect();
            let count = cubes_to_burn.len();

            // Brief write lock: drain cubes from registry + get client
            let (drained, total_edn, client) = {
                let mut ge = game_engine.write().await;
                let (drained, total_edn) = ge.drain_cubes(&cubes_to_burn);
                let client = ge.game_client().clone();
                (drained, total_edn, client)
            };
            // Write lock released — HTTP calls run without any lock

            match GameEngine::execute_burn_batch(
                &client,
                &self.wallet.private_key_b64,
                cubes_to_burn.clone(),
            )
            .await
            {
                Ok(()) => {
                    let edn_str = format!("{:.10}", total_edn);
                    tracing::info!(
                        "[{}] Batch burned {} cubes → expected ~{} EDN (via contract, async)",
                        self.name, count, edn_str,
                    );
                    SIM_BURN_BATCHES
                        .with_label_values(&[group_of(&self.name)])
                        .inc();
                    SIM_CUBES_BURNED
                        .with_label_values(&[group_of(&self.name)])
                        .inc_by(count as u64);
                    SIM_TX_SENT
                        .with_label_values(&[group_of(&self.name), "cube_burn"])
                        .inc();
                    // Start burn cooldown — wait for fee_distribution to deliver EDN.
                    // Jitter spreads the next-burn moment across the fleet so
                    // 100 agents don't synchronise on the same tick after a
                    // shared event (e.g. all bootstrapping at once).
                    self.burn_cooldown = Self::jittered_cooldown(
                        gc.burn_cooldown_ticks,
                        gc.burn_cooldown_jitter_pct,
                    );
                    let _ = ctx.metrics_tx.try_send(MetricEvent::TransactionSent {
                        agent_name: self.name.clone(),
                        block_id: format!("batch-burn:{}", count),
                        amount: format!("{} EDN (expected)", edn_str),
                        latency: std::time::Duration::from_millis(0),
                    });
                    ctx.comms
                        .broadcast(
                            &self.name,
                            AgentMessage::Info {
                                from: self.name.clone(),
                                data: serde_json::json!({
                                    "action": "batch_burn",
                                    "cubes": count,
                                    "edn_expected": edn_str,
                                }),
                            },
                        )
                        .await;
                }
                Err(e) => {
                    tracing::warn!(
                        "[{}] Failed to batch burn {} cubes: {:#}",
                        self.name, count, e
                    );
                    SIM_TX_FAILED
                        .with_label_values(&[group_of(&self.name), "cube_burn", "other"])
                        .inc();
                    // Restore cubes in registry + local list
                    {
                        let mut ge = game_engine.write().await;
                        ge.restore_cubes(drained);
                    }
                    self.cube_ids = cubes_to_burn;
                }
            }
        } else {
            // No cubes — check real EDN balance from game ledger UTXOs
            let (client, edenite_asset_id) =
                self.ensure_game_cache(game_engine).await.clone();

            let edn_balance = match client
                .token_balance(&self.wallet.address, &edenite_asset_id)
                .await
            {
                Ok(bal_str) => {
                    let bal = bal_str.parse::<f64>().unwrap_or(0.0);
                    if bal > 0.0 {
                        tracing::info!(
                            "[{}] EDN balance check: {:.10} (threshold: {:.10})",
                            self.name, bal, EDN_SEND_THRESHOLD
                        );
                    }
                    bal
                }
                Err(e) => {
                    tracing::warn!(
                        "[{}] EDN balance query FAILED: {:#} — defaulting to 0",
                        self.name, e
                    );
                    0.0
                }
            };

            if edn_balance >= EDN_SEND_THRESHOLD {
                // ── Phase 2: SEND EDN to random peers (real UTXO transfers) ──
                // Loop edn_sends_per_tick times — same pattern as PMS sends_per_tick.
                // Sequential sends (UTXO chain from same wallet), break on error.
                let edn_sends = gc.edn_sends_per_tick.max(1);
                tracing::info!(
                    "[{}] >>> PHASE 2: Sending EDN ×{} (balance={:.10})",
                    self.name, edn_sends, edn_balance
                );

                let min_frac = gc.edn_send_min_pct / 100.0;
                let max_frac = gc.edn_send_max_pct / 100.0;
                let mut edn_ok: u32 = 0;

                for round in 0..edn_sends {
                    // Re-query balance for accurate UTXO tracking between sends
                    let current_bal = if round == 0 {
                        edn_balance
                    } else {
                        match client
                            .token_balance(&self.wallet.address, &edenite_asset_id)
                            .await
                        {
                            Ok(b) => b.parse::<f64>().unwrap_or(0.0),
                            Err(_) => break,
                        }
                    };
                    if current_bal < EDN_SEND_THRESHOLD {
                        break;
                    }

                    let peers = ctx.peer_registry.read().await;
                    let others: Vec<_> =
                        peers.iter().filter(|p| p.name != self.name).collect();
                    if others.is_empty() {
                        break;
                    }
                    let target = {
                        let mut rng = rand::rng();
                        let idx: usize = rng.random_range(0..others.len());
                        others[idx].clone()
                    };
                    drop(peers);

                    let fraction = {
                        let mut rng = rand::rng();
                        rng.random_range(min_frac..max_frac)
                    };
                    let send_amount = current_bal * fraction;
                    let amount_str = format!("{:.10}", send_amount);

                    match client
                        .send_simple(&SendSimpleRequest {
                            private_key_b64: self.wallet.private_key_b64.clone(),
                            to: target.address.clone(),
                            amount: amount_str.clone(),
                            asset_id: Some(edenite_asset_id.clone()),
                        })
                        .await
                    {
                        Ok(resp) => {
                            let block_id =
                                resp.data.block_id.as_deref().unwrap_or("?");
                            let transfer_fee =
                                resp.data.transfer_fee.as_deref().unwrap_or("0");
                            tracing::info!(
                                "[{}] EDN send {}/{}: {} → {} (bal={:.10}, fee={})",
                                self.name, round + 1, edn_sends,
                                amount_str, target.name, current_bal, transfer_fee
                            );
                            let _ =
                                ctx.metrics_tx.try_send(MetricEvent::TransactionSent {
                                    agent_name: self.name.clone(),
                                    block_id: format!("edn-send:{}", &target.name),
                                    amount: format!("{} EDN", amount_str),
                                    latency: std::time::Duration::from_millis(0),
                                });
                            SIM_TX_SENT
                                .with_label_values(&[
                                    group_of(&self.name),
                                    "edn_send",
                                ])
                                .inc();
                            ctx.comms
                                .send_to(
                                    &target.name,
                                    AgentMessage::TxNotification {
                                        from: self.name.clone(),
                                        to: target.name.clone(),
                                        amount: format!("{} EDN", amount_str),
                                        block_id: block_id.to_string(),
                                    },
                                )
                                .await;
                            edn_ok += 1;
                        }
                        Err(e) => {
                            let reason = classify_error(&e);
                            SIM_TX_FAILED
                                .with_label_values(&[
                                    group_of(&self.name),
                                    "edn_send",
                                    reason,
                                ])
                                .inc();
                            tracing::warn!(
                                "[{}] EDN send {}/{} failed: {:#}",
                                self.name, round + 1, edn_sends, e
                            );
                            break;
                        }
                    }
                }

                if edn_ok > 1 {
                    tracing::info!(
                        "[{}] Phase 2 done: {}/{} EDN sends",
                        self.name, edn_ok, edn_sends
                    );
                }
            } else if self.burn_cooldown == 0 {
                // ── Phase 3: RE-MINT cubes (only if cooldown expired) ──
                // Two modes:
                //   * Progressive (`target_cubes = Some(N)`): mint
                //     `mint_per_tick` cubes — small steady drip toward N.
                //   * Legacy (None): bulk-mint a random batch from
                //     `cubes_remint_min..=cubes_remint_max`.
                let cubes_to_mint = if let Some(target) = progressive_target {
                    let current = self.cube_ids.len();
                    if current >= target.max(1) {
                        // Reached target while EDN was being sent — next
                        // tick will trigger a burn. Skip mint this tick.
                        return Ok(());
                    }
                    gc.mint_per_tick.max(1).min(target - current)
                } else {
                    let mut rng = rand::rng();
                    rng.random_range(gc.cubes_remint_min..=gc.cubes_remint_max)
                };
                tracing::info!(
                    "[{}] cubes={}/{}, EDN {:.10} < threshold → minting {} cubes",
                    self.name,
                    self.cube_ids.len(),
                    progressive_target.map(|t| t as i64).unwrap_or(-1),
                    edn_balance,
                    cubes_to_mint
                );

                // Generate specs without any lock (pure RNG)
                let specs = GameEngine::generate_mint_specs(cubes_to_mint);

                // HTTP calls without any lock
                let (minted, ok_count, _err_count) = GameEngine::execute_mints_parallel(
                    &client,
                    &specs,
                    &self.wallet.address,
                    &self.wallet.x25519_pub_hex,
                )
                .await;

                // Brief write lock: register minted cubes in registry
                {
                    let mut ge = game_engine.write().await;
                    let ids = ge.register_minted(minted);
                    self.cube_ids.extend(ids);
                }
                // Write lock released

                SIM_CUBES_MINTED
                    .with_label_values(&[group_of(&self.name)])
                    .inc_by(ok_count as u64);
                SIM_TX_SENT
                    .with_label_values(&[group_of(&self.name), "cube_mint"])
                    .inc_by(ok_count as u64);
                let failed_mints = cubes_to_mint.saturating_sub(ok_count);
                if failed_mints > 0 {
                    SIM_TX_FAILED
                        .with_label_values(&[
                            group_of(&self.name),
                            "cube_mint",
                            "other",
                        ])
                        .inc_by(failed_mints as u64);
                }
                tracing::info!(
                    "[{}] Parallel mint done: {}/{} cubes",
                    self.name, ok_count, cubes_to_mint
                );
                if ok_count > 0 {
                    ctx.comms
                        .broadcast(
                            &self.name,
                            AgentMessage::Info {
                                from: self.name.clone(),
                                data: serde_json::json!({
                                    "action": "remint",
                                    "cubes_minted": ok_count,
                                }),
                            },
                        )
                        .await;
                }
            } else {
                tracing::debug!(
                    "[{}] Burn cooldown: {} ticks remaining, waiting for EDN delivery",
                    self.name, self.burn_cooldown
                );
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

            let _ = ctx.metrics_tx.try_send(MetricEvent::BalanceUpdate {
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
                    let _ = ctx.metrics_tx.try_send(MetricEvent::AgentError {
                        agent_name: self.name.clone(),
                        error: format!("refuel failed: {:#}", e),
                    });
                    return Ok(());
                }
            }
        }

        // Game tick every tick (frequency controlled by interval_ms)
        self.game_tick(ctx).await?;

        // ── PMS send loop: sends_per_tick sequential transactions ──
        // Each send depends on the previous one's UTXO output being committed,
        // which happens synchronously in wallet_send_simple.
        for _round in 0..self.sends_per_tick {
            // Decide whether to send PMS this round
            let (should_send, peer_idx, amount_str) = {
                let mut rng = rand::rng();
                let send = rng.random::<f64>() <= self.send_probability;
                let idx: usize = rng.random_range(0..usize::MAX);
                let amount = rng.random_range(self.min_amount..=self.max_amount);
                (send, idx, format!("{:.2}", amount))
            };

            if !should_send {
                continue;
            }

            // Pick a random peer (not self)
            let peers = ctx.peer_registry.read().await;
            let others: Vec<_> = peers.iter().filter(|p| p.name != self.name).collect();
            if others.is_empty() {
                break;
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

                    let _ = ctx.metrics_tx.try_send(MetricEvent::TransactionSent {
                        agent_name: self.name.clone(),
                        block_id,
                        amount: amount_str,
                        latency: resp.latency,
                    });
                    SIM_TX_SENT
                        .with_label_values(&[group_of(&self.name), "pms_send"])
                        .inc();
                }
                Err(e) => {
                    let reason = classify_error(&e);
                    SIM_TX_FAILED
                        .with_label_values(&[group_of(&self.name), "pms_send", reason])
                        .inc();
                    let _ = ctx.metrics_tx.try_send(MetricEvent::AgentError {
                        agent_name: self.name.clone(),
                        error: format!("{:#}", e),
                    });
                    // Stop sending more in this tick on error (likely UTXO exhaustion)
                    break;
                }
            }
        }

        Ok(())
    }
}
