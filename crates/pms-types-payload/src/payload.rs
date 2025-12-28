use serde::{Serialize, Deserialize};

use crate::{Transaction, Reward};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Payload {
    Genesis,
    Reward(Reward),
    Transaction(Transaction),
}