use crate::agent::{Agent, AgentContext};
use crate::comms::types::AgentMessage;
use crate::error::SimResult;
use crate::gemini::AgentDirective;
use crate::metrics::MetricEvent;
use crate::types::{SendSimpleRequest, WalletInfo};
use rand::Rng;
use std::collections::VecDeque;
use tokio::sync::mpsc;

/// AI-powered agent driven by Gemini Pro (hybrid mode)
pub struct SmartAgent {
    name: String,
    wallet: WalletInfo,
    system_prompt: String,
    /// Current directive from Gemini
    directive: Option<AgentDirective>,
    /// Ticks since last Gemini call
    ticks_since_ai: u32,
    /// Call Gemini every N ticks
    ai_interval: u32,
    /// Inbox for P2P messages
    inbox: mpsc::Receiver<AgentMessage>,
    /// Buffered messages received since last AI call
    pending_messages: Vec<AgentMessage>,
    /// Recent action history (for context)
    action_history: VecDeque<String>,
    /// Current balance (cached)
    cached_balance: String,
}

impl SmartAgent {
    pub fn new(
        name: String,
        wallet: WalletInfo,
        system_prompt: String,
        ai_interval: u32,
        inbox: mpsc::Receiver<AgentMessage>,
    ) -> Self {
        Self {
            name,
            wallet,
            system_prompt,
            directive: None,
            ticks_since_ai: 0,
            ai_interval,
            inbox,
            pending_messages: Vec::new(),
            action_history: VecDeque::with_capacity(20),
            cached_balance: "0".to_string(),
        }
    }

    /// Drain inbox into pending_messages
    fn collect_messages(&mut self) {
        while let Ok(msg) = self.inbox.try_recv() {
            self.pending_messages.push(msg);
        }
    }

    /// Build context string for Gemini
    async fn build_context(&self, ctx: &AgentContext) -> String {
        let peers = ctx.peer_registry.read().await;
        let peer_list: Vec<String> = peers
            .iter()
            .filter(|p| p.name != self.name)
            .map(|p| format!("  - {} ({}...)", p.name, &p.address[..20.min(p.address.len())]))
            .collect();

        let messages: Vec<String> = self
            .pending_messages
            .iter()
            .map(|m| format!("  - {}", m.summary()))
            .collect();

        let history: Vec<String> = self.action_history.iter().cloned().collect();

        format!(
            "Agent: {name}\nBalance: {balance} PMS\nAddress: {addr}\n\n\
             Peers:\n{peers}\n\n\
             Messages reçus:\n{msgs}\n\n\
             Historique récent:\n{hist}",
            name = self.name,
            balance = self.cached_balance,
            addr = self.wallet.address,
            peers = if peer_list.is_empty() {
                "  (aucun)".to_string()
            } else {
                peer_list.join("\n")
            },
            msgs = if messages.is_empty() {
                "  (aucun)".to_string()
            } else {
                messages.join("\n")
            },
            hist = if history.is_empty() {
                "  (aucune)".to_string()
            } else {
                history.join("\n")
            },
        )
    }

    fn log_action(&mut self, action: &str) {
        if self.action_history.len() >= 20 {
            self.action_history.pop_front();
        }
        self.action_history.push_back(action.to_string());
    }

    /// Build a self-diagnosis explanation when an error occurs
    fn diagnose_error(&self, error: &str) -> String {
        let bal_info = format!("Mon solde actuel est de {} PMS", self.cached_balance);
        let last_actions = if self.action_history.is_empty() {
            "aucune action récente".to_string()
        } else {
            let recent: Vec<_> = self.action_history.iter().rev().take(3).collect();
            recent.into_iter().cloned().collect::<Vec<_>>().join(", ")
        };
        let directive_info = match &self.directive {
            Some(d) => format!("Je tentais d'exécuter: {:?}", d),
            None => "Je n'avais pas de directive en cours".to_string(),
        };
        let pending = if self.pending_messages.is_empty() {
            "aucun message en attente".to_string()
        } else {
            format!("{} messages en attente", self.pending_messages.len())
        };

        format!(
            "J'ai besoin d'aide ! {bal_info}. {directive_info}. \
             Mes dernières actions: {last_actions}. \
             {pending}. L'erreur exacte: {error}"
        )
    }

    /// Broadcast an error message with self-diagnosis to all agents
    async fn broadcast_error(&self, ctx: &AgentContext, error: &str) {
        let explanation = self.diagnose_error(error);
        ctx.comms
            .broadcast(
                &self.name,
                AgentMessage::Error {
                    from: self.name.clone(),
                    error: error.to_string(),
                    explanation,
                },
            )
            .await;
    }

    /// Execute the current directive (errors are handled internally via broadcast)
    async fn execute_directive(&mut self, ctx: &AgentContext) {
        let directive = match &self.directive {
            Some(d) => d.clone(),
            None => return,
        };

        match directive {
            AgentDirective::Send {
                to_agent,
                amount,
                reason,
            } => {
                // Find the target address
                let peers = ctx.peer_registry.read().await;
                let target = peers.iter().find(|p| p.name == to_agent);
                let to_address = match target {
                    Some(p) => p.address.clone(),
                    None => {
                        // Pick random peer if target not found
                        let others: Vec<_> =
                            peers.iter().filter(|p| p.name != self.name).collect();
                        if others.is_empty() {
                            return;
                        }
                        let mut rng = rand::rng();
                        others[rng.random_range(0..others.len())]
                            .address
                            .clone()
                    }
                };
                drop(peers);

                match ctx
                    .client
                    .send_simple(&SendSimpleRequest {
                        private_key_b64: self.wallet.private_key_b64.clone(),
                        to: to_address.clone(),
                        amount: amount.clone(),
                        asset_id: None,
                    })
                    .await
                {
                    Ok(resp) => {
                        let block_id = resp.data.block_id.unwrap_or_default();

                        ctx.comms
                            .send_to(
                                &to_agent,
                                AgentMessage::TxNotification {
                                    from: self.name.clone(),
                                    to: to_agent.clone(),
                                    amount: amount.clone(),
                                    block_id: block_id.clone(),
                                },
                            )
                            .await;

                        let _ = ctx.metrics_tx.try_send(MetricEvent::TransactionSent {
                            agent_name: self.name.clone(),
                            block_id: block_id.clone(),
                            amount: amount.clone(),
                            latency: resp.latency,
                        });

                        self.log_action(&format!(
                            "Sent {} PMS to {} ({})",
                            amount, to_agent, reason
                        ));
                    }
                    Err(e) => {
                        let err_str = format!(
                            "Échec d'envoi de {} PMS à {}: {}",
                            amount, to_agent, e
                        );
                        self.broadcast_error(ctx, &err_str).await;
                        let _ = ctx.metrics_tx.try_send(MetricEvent::AgentError {
                            agent_name: self.name.clone(),
                            error: err_str.clone(),
                        });
                        self.log_action(&format!("ERREUR: {}", err_str));
                    }
                }
            }
            AgentDirective::Wait { reason, .. } => {
                self.log_action(&format!("Waiting ({})", reason));
            }
            AgentDirective::Observe { what } => {
                let result = match what.as_str() {
                    "tips" => {
                        match ctx.client.get_tips(5).await {
                            Ok(tips) => {
                                let _ = ctx.metrics_tx.try_send(MetricEvent::TipsCount(tips.len()));
                                self.log_action(&format!("Observed {} tips", tips.len()));
                                Ok(())
                            }
                            Err(e) => Err(format!("Impossible de récupérer les tips: {}", e)),
                        }
                    }
                    "supply" => {
                        match ctx.client.get_supply().await {
                            Ok(supply) => {
                                let _ = ctx.metrics_tx.try_send(MetricEvent::SupplyUpdate {
                                    circulating: supply.circulating_supply.clone(),
                                    utxo_count: supply.utxo_count,
                                });
                                self.log_action(&format!(
                                    "Observed supply: {}",
                                    supply.circulating_supply
                                ));
                                Ok(())
                            }
                            Err(e) => Err(format!("Impossible de récupérer la supply: {}", e)),
                        }
                    }
                    _ => {
                        match ctx.client.balance(&self.wallet.address).await {
                            Ok(bal) => {
                                self.cached_balance = bal.clone();
                                let _ = ctx.metrics_tx.try_send(MetricEvent::BalanceUpdate {
                                    agent_name: self.name.clone(),
                                    balance: bal,
                                });
                                self.log_action("Observed balance");
                                Ok(())
                            }
                            Err(e) => Err(format!("Impossible de récupérer mon solde: {}", e)),
                        }
                    }
                };

                if let Err(err_str) = result {
                    self.broadcast_error(ctx, &err_str).await;
                    let _ = ctx.metrics_tx.try_send(MetricEvent::AgentError {
                        agent_name: self.name.clone(),
                        error: err_str.clone(),
                    });
                    self.log_action(&format!("ERREUR: {}", err_str));
                }
            }
            AgentDirective::Message { to_agent, content } => {
                ctx.comms
                    .send_to(
                        &to_agent,
                        AgentMessage::Text {
                            from: self.name.clone(),
                            content: content.clone(),
                        },
                    )
                    .await;
                self.log_action(&format!("Messaged {}: {}", to_agent, content));
            }
        }
    }
}

#[async_trait::async_trait]
impl Agent for SmartAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn wallet(&self) -> &WalletInfo {
        &self.wallet
    }

    async fn tick(&mut self, ctx: &AgentContext) -> SimResult<()> {
        // 1. Collect incoming messages
        self.collect_messages();

        // 2. Update balance cache periodically
        if self.ticks_since_ai == 0 {
            self.cached_balance =
                ctx.client.balance(&self.wallet.address).await.unwrap_or("0".to_string());
        }

        // 3. Call Gemini every N ticks
        self.ticks_since_ai += 1;
        if self.ticks_since_ai >= self.ai_interval {
            self.ticks_since_ai = 0;

            let context = self.build_context(ctx).await;
            let gemini = match &ctx.gemini {
                Some(g) => g,
                None => return Ok(()),
            };
            match gemini.decide(&self.system_prompt, &context).await {
                Ok(directive) => {
                    let _ = ctx.metrics_tx.try_send(MetricEvent::GeminiDecision {
                        agent_name: self.name.clone(),
                        directive: format!("{:?}", directive),
                    });
                    self.directive = Some(directive);
                    self.pending_messages.clear();
                }
                Err(e) => {
                    let err_str = format!("Gemini API error: {:#}", e);
                    tracing::warn!("[{}] {}", self.name, err_str);
                    self.broadcast_error(ctx, &err_str).await;
                    let _ = ctx.metrics_tx.try_send(MetricEvent::AgentError {
                        agent_name: self.name.clone(),
                        error: err_str,
                    });
                    // Keep previous directive
                }
            }
        }

        // 4. Execute current directive
        self.execute_directive(ctx).await;

        Ok(())
    }
}
