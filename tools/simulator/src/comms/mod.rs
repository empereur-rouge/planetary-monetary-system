pub mod types;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use types::AgentMessage;

/// Per-agent inbox capacity (bounded to prevent OOM)
const AGENT_INBOX_CAPACITY: usize = 256;

/// Central router that dispatches messages between agents
#[derive(Clone)]
pub struct CommsRouter {
    inboxes: Arc<RwLock<HashMap<String, mpsc::Sender<AgentMessage>>>>,
    /// All messages for TUI display (bounded)
    pub global_log: mpsc::Sender<AgentMessage>,
}

impl CommsRouter {
    pub fn new(global_log: mpsc::Sender<AgentMessage>) -> Self {
        Self {
            inboxes: Arc::new(RwLock::new(HashMap::new())),
            global_log,
        }
    }

    /// Register an agent and return its inbox receiver
    pub async fn register(&self, name: &str) -> mpsc::Receiver<AgentMessage> {
        let (tx, rx) = mpsc::channel(AGENT_INBOX_CAPACITY);
        self.inboxes.write().await.insert(name.to_string(), tx);
        rx
    }

    /// Send a message to a specific agent
    pub async fn send_to(&self, target: &str, msg: AgentMessage) {
        // Log globally for TUI (non-critical, drop if full)
        let _ = self.global_log.try_send(msg.clone());

        // Deliver to target's inbox (drop if full to avoid blocking agents)
        let inboxes = self.inboxes.read().await;
        if let Some(tx) = inboxes.get(target) {
            if tx.try_send(msg).is_err() {
                tracing::trace!("agent inbox full or closed for {target}");
            }
        }
    }

    /// Broadcast a message to all agents except the sender
    pub async fn broadcast(&self, sender: &str, msg: AgentMessage) {
        let _ = self.global_log.try_send(msg.clone());

        let inboxes = self.inboxes.read().await;
        for (name, tx) in inboxes.iter() {
            if name != sender {
                if tx.try_send(msg.clone()).is_err() {
                    tracing::trace!("agent inbox full or closed for {name}");
                }
            }
        }
    }

    /// List all registered agent names
    pub async fn agents(&self) -> Vec<String> {
        self.inboxes.read().await.keys().cloned().collect()
    }
}
