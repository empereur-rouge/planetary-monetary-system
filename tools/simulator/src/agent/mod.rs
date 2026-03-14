pub mod coordinator;
pub mod funder;
pub mod observer;
pub mod random;
pub mod smart;

use crate::client::DagClient;
use crate::comms::CommsRouter;
use crate::error::SimResult;
use crate::game::GameEngine;
use crate::gemini::GeminiClient;
use crate::metrics::MetricEvent;
use crate::types::WalletInfo;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

/// Shared context for all agents
pub struct AgentContext {
    pub client: DagClient,
    pub gemini: Option<GeminiClient>,
    pub comms: CommsRouter,
    pub metrics_tx: mpsc::Sender<MetricEvent>,
    /// All agent names + addresses for peer discovery
    pub peer_registry: Arc<RwLock<Vec<PeerInfo>>>,
    pub cancel: CancellationToken,
    /// Optional game engine for Edenite cube NFTs
    pub game_engine: Option<Arc<RwLock<GameEngine>>>,
    /// Coordinator wallet for PMS distribution (agents request refuel from coordinator)
    pub coordinator_wallet: Option<WalletInfo>,
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub name: String,
    pub address: String,
}

/// Trait for agent behaviors
#[async_trait::async_trait]
pub trait Agent: Send + 'static {
    fn name(&self) -> &str;
    fn wallet(&self) -> &WalletInfo;

    /// Perform one tick of behavior.
    /// Errors are logged but not fatal — the agent continues.
    async fn tick(&mut self, ctx: &AgentContext) -> SimResult<()>;
}

/// Handle to a running agent task
pub struct AgentHandle {
    pub name: String,
    pub address: String,
    pub join: tokio::task::JoinHandle<()>,
}

/// Spawn an agent as a tokio task with random initial jitter to avoid thundering herd
pub fn spawn_agent(
    mut agent: Box<dyn Agent>,
    ctx: Arc<AgentContext>,
    interval_ms: u64,
) -> AgentHandle {
    let name = agent.name().to_string();
    let address = agent.wallet().address.clone();

    // Random jitter: 0..interval_ms to stagger agent starts
    let jitter_ms = {
        use rand::Rng;
        let mut rng = rand::rng();
        rng.random_range(0..interval_ms)
    };

    let join = tokio::spawn(async move {
        // Stagger start to avoid all agents hitting the gateway simultaneously
        tokio::time::sleep(std::time::Duration::from_millis(jitter_ms)).await;

        let mut interval =
            tokio::time::interval(std::time::Duration::from_millis(interval_ms));

        loop {
            tokio::select! {
                _ = ctx.cancel.cancelled() => break,
                _ = interval.tick() => {
                    match agent.tick(&ctx).await {
                        Ok(()) => {}
                        Err(e) => {
                            let _ = ctx.metrics_tx.try_send(MetricEvent::AgentError {
                                agent_name: agent.name().to_string(),
                                error: format!("{:#}", e),
                            });
                        }
                    }
                }
            }
        }
    });

    AgentHandle {
        name,
        address,
        join,
    }
}
