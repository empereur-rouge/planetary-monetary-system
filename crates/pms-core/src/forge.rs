use crate::DagRef;

use pms_types::{Block, PayloadEnvelope};
use pms_utils::compute_block_id;
use pms_wire::WireBlock;

/// Forge un block localement à partir du DAG en RAM:
/// - choisit les parents via `Dag::forge_block`
/// - applique le `compute_id_adapter`
/// - ne persiste rien, ne modifie pas le store.
pub async fn forge_block_with(
    dag: &DagRef,
    payload: Option<PayloadEnvelope>,
    _kind: &str, // juste pour logs si tu veux plus tard
) -> Block {
    // dag is Arc<ConcurrentDag>, no lock needed
    // We use unwrap() here to match previous signature which returned Block.
    // In production, we should propagate errors.
    dag.forge_block(
        payload,
        0, // extra_nonce (difficulty_leading_zeros in ConcurrentDag::forge_block seems to be the param 2?)
        // Wait, ConcurrentDag::forge_block args are: (payload, difficulty_leading_zeros, compute_id)
        compute_block_id, // correct function matching signature
    )
    .expect("forge_block failed")
}

pub fn to_wire(forged: &Block) -> WireBlock {
    WireBlock {
        id: forged.id.clone(),
        parents: forged.parents.clone(),
        payload_json: serde_json::to_string(&forged.payload).ok(),
        nonce: forged.nonce,
        network_id: "".to_string(),
        protocol_version: 0,
        signer_pk_hex: "".to_string(),
        signature_hex: "".to_string(),
        metadata: forged.metadata.clone(),
    }
}
