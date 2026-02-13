use serde::{Deserialize, Serialize};

/// Messages exchanged between agents via the local P2P comms system
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
    /// Free-form text message
    Text { from: String, content: String },
    /// Notification that a TX was sent
    TxNotification {
        from: String,
        to: String,
        amount: String,
        block_id: String,
    },
    /// Generic info broadcast
    Info {
        from: String,
        data: serde_json::Value,
    },
    /// Error with agent self-diagnosis (displayed in red on web page)
    Error {
        from: String,
        error: String,
        explanation: String,
    },
}

impl AgentMessage {
    pub fn sender(&self) -> &str {
        match self {
            AgentMessage::Text { from, .. } => from,
            AgentMessage::TxNotification { from, .. } => from,
            AgentMessage::Info { from, .. } => from,
            AgentMessage::Error { from, .. } => from,
        }
    }

    pub fn summary(&self) -> String {
        match self {
            AgentMessage::Text { from, content } => {
                format!("[{from}]: {content}")
            }
            AgentMessage::TxNotification {
                from,
                to,
                amount,
                block_id,
            } => {
                let short_id = if block_id.len() > 12 {
                    &block_id[..12]
                } else {
                    block_id
                };
                format!("[{from} → {to}]: {amount} PMS (block {short_id}...)")
            }
            AgentMessage::Info { from, data } => {
                format!("[{from}]: info {data}")
            }
            AgentMessage::Error {
                from,
                error,
                explanation,
            } => {
                format!("[{from}] ERROR: {error} — {explanation}")
            }
        }
    }
}
