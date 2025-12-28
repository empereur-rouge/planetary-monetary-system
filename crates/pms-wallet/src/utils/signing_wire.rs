
use serde::Serialize;
use pms_wire::WireBlock;

/// Vue canonique pour signature (ordre de champs figé)
#[derive(Serialize)]
struct WireBlockSignView<'a> {
    id: &'a str,
    parents: &'a [String],
    payload_json: &'a Option<String>,
    nonce: u64,
    network_id: &'a str,
    protocol_version: u32,
    signer_pk_hex: &'a str,
}

/// Construit le message déterministe à signer pour un WireBlock.
pub fn canonical_wireblock_message(wb: &WireBlock) -> String {
    let view = WireBlockSignView {
        id: &wb.id,
        parents: &wb.parents,
        payload_json: &wb.payload_json,
        nonce: wb.nonce,
        network_id: &wb.network_id,
        protocol_version: wb.protocol_version as u32,
        signer_pk_hex: &wb.signer_pk_hex,
    };

    serde_json::to_string(&view)
        .expect("WireBlockSignView must always be serializable")
}