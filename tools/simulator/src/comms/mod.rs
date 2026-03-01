pub mod types;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use types::AgentMessage;

/// Central router that dispatches messages between agents
#[derive(Clone)]
pub struct CommsRouter {
    inboxes: Arc<RwLock<HashMap<String, mpsc::UnboundedSender<AgentMessage>>>>,
    /// All messages for TUI display
    pub global_log: mpsc::UnboundedSender<AgentMessage>,
}

impl CommsRouter {
    pub fn new(global_log: mpsc::UnboundedSender<AgentMessage>) -> Self {
        Self {
            inboxes: Arc::new(RwLock::new(HashMap::new())),
            global_log,
        }
    }

    /// Register an agent and return its inbox receiver
    pub async fn register(&self, name: &str) -> mpsc::UnboundedReceiver<AgentMessage> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inboxes.write().await.insert(name.to_string(), tx);
        rx
    }

    /// Send a message to a specific agent
    pub async fn send_to(&self, target: &str, msg: AgentMessage) {
        // Log globally for TUI (non-critical, ignore errors during shutdown)
        let _ = self.global_log.send(msg.clone());

        // Deliver to target's inbox
        let inboxes = self.inboxes.read().await;
        if let Some(tx) = inboxes.get(target) {
            if tx.send(msg).is_err() {
                tracing::trace!("agent inbox closed for {target}");
            }
        }
    }

    /// Broadcast a message to all agents except the sender
    pub async fn broadcast(&self, sender: &str, msg: AgentMessage) {
        let _ = self.global_log.send(msg.clone());

        let inboxes = self.inboxes.read().await;
        for (name, tx) in inboxes.iter() {
            if name != sender {
                if tx.send(msg.clone()).is_err() {
                    tracing::trace!("agent inbox closed for {name}");
                }
            }
        }
    }

    /// List all registered agent names
    pub async fn agents(&self) -> Vec<String> {
        self.inboxes.read().await.keys().cloned().collect()
    }
}
