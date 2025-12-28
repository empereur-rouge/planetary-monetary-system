use pms_utils::compute_id_adapter;
use pms_types::{Block, PayloadEnvelope};
use pms_wire::WireBlock;
use crate::DagRef;

/// Forge un block localement à partir du DAG en RAM:
/// - choisit les parents via `Dag::forge_block`
/// - applique le `compute_id_adapter`
/// - ne persiste rien, ne modifie pas le store.
pub async fn forge_block_with(
    dag: &DagRef,
    payload: Option<PayloadEnvelope>,
    _kind: &str, // juste pour logs si tu veux plus tard
) -> Block {
    let mut d = dag.lock().await;

    // tu as déjà une API de ce type dans tes tests:
    // d.forge_block(payload, extra_nonce, compute_id_adapter)
    let block = d.forge_block(
        payload,
        0,                  // extra_nonce
        compute_id_adapter, // même fonction que dans tes tests
    );

    block
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
    }
}