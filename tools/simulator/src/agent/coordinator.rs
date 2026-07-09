use crate::agent::{Agent, AgentContext};
use crate::backoff::{log_throttle, WARN_THROTTLE_PERIOD};
use crate::error::SimResult;
use crate::metrics::MetricEvent;
use crate::types::{SendSimpleRequest, WalletInfo};
use rand::Rng;

/// Agent that uses the coordinator wallet to send random PMS to peers.
/// No refuel (funded by network fees), no game loop, no inbox.
pub struct CoordinatorAgent {
    name: String,
    wallet: WalletInfo,
    cached_balance: f64,
    tick_count: u32,
    min_amount: f64,
    max_amount: f64,
    send_probability: f64,
}

impl CoordinatorAgent {
    pub fn new(
        name: String,
        wallet: WalletInfo,
        min_amount: f64,
        max_amount: f64,
        send_probability: f64,
    ) -> Self {
        Self {
            name,
            wallet,
            cached_balance: 0.0,
            tick_count: 0,
            min_amount,
            max_amount,
            send_probability,
        }
    }
}

#[async_trait::async_trait]
impl Agent for CoordinatorAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn wallet(&self) -> &WalletInfo {
        &self.wallet
    }

    async fn tick(&mut self, ctx: &AgentContext) -> SimResult<()> {
        self.tick_count += 1;

        // Refresh balance every 5 ticks
        if self.tick_count % 5 == 0 || self.tick_count == 1 {
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

        // Check if we have enough balance to send.
        // État durable (wallet à sec) → warn throttlé : sans lui, ce warn
        // partait à CHAQUE tick tant que le wallet n'était pas refinancé.
        if self.cached_balance < self.min_amount {
            if let Some(suppressed) =
                log_throttle::allow("coordinator_low_balance", WARN_THROTTLE_PERIOD)
            {
                tracing::warn!(
                    "[{}] Balance {:.2} PMS < min_amount {:.2}, skipping send — {} warns similaires étouffés sur {:?}",
                    self.name,
                    self.cached_balance,
                    self.min_amount,
                    suppressed,
                    WARN_THROTTLE_PERIOD
                );
            }
            return Ok(());
        }

        // Decide whether to send this tick
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
                tracing::info!(
                    "[{}] Sent {} PMS → {} (block {})",
                    self.name,
                    amount_str,
                    target.name,
                    &block_id[..16.min(block_id.len())]
                );

                let _ = ctx.metrics_tx.try_send(MetricEvent::TransactionSent {
                    agent_name: self.name.clone(),
                    block_id,
                    amount: amount_str,
                    latency: resp.latency,
                });
            }
            Err(e) => {
                // Erreur d'ÉTAT (solde insuffisant côté serveur) : throttlé —
                // même cause racine que le check de solde ci-dessus, le cache
                // de balance (rafraîchi tous les 5 ticks) peut être en retard.
                if e.is_state_error() {
                    if let Some(suppressed) =
                        log_throttle::allow("coordinator_send_state_error", WARN_THROTTLE_PERIOD)
                    {
                        tracing::warn!(
                            "[{}] Failed to send {} PMS to {} (erreur d'état): {:#} — {} warns similaires étouffés sur {:?}",
                            self.name,
                            amount_str,
                            target.name,
                            e,
                            suppressed,
                            WARN_THROTTLE_PERIOD
                        );
                    }
                } else {
                    tracing::warn!(
                        "[{}] Failed to send {} PMS to {}: {:#}",
                        self.name,
                        amount_str,
                        target.name,
                        e
                    );
                }
                let _ = ctx.metrics_tx.try_send(MetricEvent::AgentError {
                    agent_name: self.name.clone(),
                    error: format!("{:#}", e),
                });
            }
        }

        Ok(())
    }
}
