use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;
use pms_network::DagAdapter;
use pms_types_block::{Block, BlockId};

/// État finalité minimal : on garde l’id du dernier milestone + l’ensemble des blocs finalisés.
#[derive(Default)]
pub struct FinalityState {
    /// blocs finalisés (ids)
    pub finalized: HashSet<String>,
    /// dernier milestone (seed pour tips deterministes)
    pub last_milestone: Option<String>,
    /// seuil de confirmations (profondeur) pour finaliser automatiquement
    pub depth_k: usize,
}

pub struct Dag {
    pub blocks: HashMap<BlockId, Block>, // consensus: id, parents, payload?, nonce
    pub children: HashMap<BlockId, u64>, // NON-consensus: children_count (local)
    pub children_idx: HashMap<BlockId, Vec<BlockId>>,
    pub net_adapter: Option<Arc<dyn DagAdapter>>,
    pub finality: FinalityState,
    pub spent_outpoints: HashSet<(String, u32)>,
}

pub type DagRef = Arc<Mutex<Dag>>;