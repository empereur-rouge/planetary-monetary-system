use crate::agent::{Agent, AgentContext};
use crate::error::SimResult;
use crate::metrics::MetricEvent;
use crate::types::WalletInfo;

/// Passive observer agent: polls DAG state without transacting
pub struct ObserverAgent {
    name: String,
    wallet: WalletInfo,
    tick_count: u64,
}

impl ObserverAgent {
    pub fn new(name: String, wallet: WalletInfo) -> Self {
        Self {
            name,
            wallet,
            tick_count: 0,
        }
    }
}

#[async_trait::async_trait]
impl Agent for ObserverAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn wallet(&self) -> &WalletInfo {
        &self.wallet
    }

    async fn tick(&mut self, ctx: &AgentContext) -> SimResult<()> {
        self.tick_count += 1;

        // Alternate between different observations
        match self.tick_count % 3 {
            0 => {
                // Poll tips
                let tips = ctx.client.get_tips(10).await?;
                let _ = ctx.metrics_tx.send(MetricEvent::TipsCount(tips.len()));
            }
            1 => {
                // Poll supply
                let supply = ctx.client.get_supply().await?;
                let _ = ctx.metrics_tx.send(MetricEvent::SupplyUpdate {
                    circulating: supply.circulating_supply,
                    utxo_count: supply.utxo_count,
                });
            }
            _ => {
                // Poll all agent balances
                let peers = ctx.peer_registry.read().await;
                for peer in peers.iter() {
                    if let Ok(bal) = ctx.client.balance(&peer.address).await {
                        let _ = ctx.metrics_tx.send(MetricEvent::BalanceUpdate {
                            agent_name: peer.name.clone(),
                            balance: bal,
                        });
                    }
                }
            }
        }

        Ok(())
    }
}
