use serde::{Deserialize, Serialize};
use pms_types_payload::{PayloadEnvelope, PlainPayload};

pub type BlockId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Block {
    pub id: BlockId,
    pub parents: Vec<String>,     // 2..N
    pub payload: Option<PayloadEnvelope>,
    pub nonce: u64,               // PoW léger (anti-spam)
}

// 💡 Métadonnée LOCALE (réseau/stocks privés), non-consensus
// - ne voyage pas sur le réseau
// - utile pour la sélection de parents et le pruning
#[derive(Debug, Clone, Default)]
pub struct BlockMeta {
    pub _children_count: u64,
}

impl Block {
    /// Crée un block “normal” (hors genesis).
    /// - `compute_id` te laisse brancher ton encodage canonique (hash).
    pub fn new(
        parents: Vec<String>,
        payload: Option<PayloadEnvelope>,
        nonce: u64,
        compute_id: impl Fn(&[String], &Option<PayloadEnvelope>, u64) -> BlockId,
    ) -> Result<Self, String> {
        if parents.is_empty() {
            return Err("Un block hors genesis doit avoir au moins 1 parent".into());
        }
        let id = compute_id(&parents, &payload, nonce);
        Ok(Self { id, parents, payload, nonce })
    }

    /// Crée le block genesis :
    /// - parents vides
    /// - payload = Genesis
    /// - nonce = 0
    pub fn genesis(
        compute_id: impl Fn(&[String], &Option<PayloadEnvelope>, u64) -> BlockId,
    ) -> Self {
        let parents = Vec::new();
        let payload = Some(PayloadEnvelope::Plain(PlainPayload::Genesis));
        let nonce = 0u64;
        let id = compute_id(&parents, &payload, nonce);
        Self { id, parents, payload, nonce }
    }
}