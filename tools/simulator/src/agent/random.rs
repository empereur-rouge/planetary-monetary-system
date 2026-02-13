use crate::agent::{Agent, AgentContext};
use crate::comms::types::AgentMessage;
use crate::error::SimResult;
use crate::metrics::MetricEvent;
use crate::types::{SendSimpleRequest, WalletInfo};
use rand::Rng;
use tokio::sync::mpsc;

/// Seuil PMS en-dessous duquel l'agent refuel (claim CUBE + burn)
const LOW_BALANCE_THRESHOLD: f64 = 10.0;
/// CUBEs a bruler lors d'un refuel (500 CUBE = 50 PMS au taux 10:1)
const REFUEL_CUBES: &str = "500.00";

/// Agent that sends random amounts to random peers — no AI needed.
/// Auto-refuels via cube claim+burn when balance drops below threshold.
pub struct RandomAgent {
    name: String,
    wallet: WalletInfo,
    inbox: mpsc::UnboundedReceiver<AgentMessage>,
    cached_balance: f64,
    tick_count: u32,
    min_amount: f64,
    max_amount: f64,
    send_probability: f64,
}

impl RandomAgent {
    pub fn new(
        name: String,
        wallet: WalletInfo,
        inbox: mpsc::UnboundedReceiver<AgentMessage>,
        min_amount: f64,
        max_amount: f64,
        send_probability: f64,
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
        }
    }

    fn drain_inbox(&mut self) {
        while self.inbox.try_recv().is_ok() {}
    }

    /// Claim CUBEs then burn them for PMS
    async fn refuel(&self, ctx: &AgentContext) -> SimResult<()> {
        let addr = &self.wallet.address;

        // 1) Claim CUBEs
        let claim_resp = ctx.client.cube_claim(addr).await?;
        let cubes = claim_resp.data.amount.unwrap_or_default();
        tracing::info!("[{}] Refuel: claimed {} CUBE", self.name, cubes);

        // Petit delai pour laisser l'UTXO se propager
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // 2) Burn CUBEs -> PMS
        let burn_resp = ctx
            .client
            .cube_burn(&self.wallet.private_key_b64, REFUEL_CUBES)
            .await?;
        let pms = burn_resp.data.pms_received.unwrap_or_default();
        tracing::info!(
            "[{}] Refuel: burned {} CUBE -> {} PMS",
            self.name,
            REFUEL_CUBES,
            pms
        );

        let _ = ctx.metrics_tx.send(MetricEvent::AgentFunded {
            agent_name: self.name.clone(),
            amount: pms,
        });

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

        // Refresh balance every 5 ticks
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

        // Auto-refuel quand le solde est trop bas
        if self.cached_balance < LOW_BALANCE_THRESHOLD {
            tracing::info!(
                "[{}] Balance {:.2} PMS < {:.2}, refueling...",
                self.name,
                self.cached_balance,
                LOW_BALANCE_THRESHOLD
            );
            match self.refuel(ctx).await {
                Ok(()) => {
                    // Re-fetch balance apres refuel
                    let bal_str = ctx
                        .client
                        .balance(&self.wallet.address)
                        .await
                        .unwrap_or_else(|_| "0".to_string());
                    self.cached_balance = bal_str.parse::<f64>().unwrap_or(0.0);
                }
                Err(e) => {
                    let _ = ctx.metrics_tx.send(MetricEvent::AgentError {
                        agent_name: self.name.clone(),
                        error: format!("refuel failed: {:#}", e),
                    });
                    return Ok(());
                }
            }
        }

        // Decide whether to send this tick (scope rng before any .await)
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

        // Send
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
