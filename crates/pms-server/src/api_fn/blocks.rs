use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose;
use hex::FromHex;
use k256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use std::sync::atomic::Ordering;

use crate::api::AppState;
use pms_network::messages::NetMsg;
use pms_storage::PutResult;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::WireBlock;

pub async fn submit_block(State(st): State<AppState>, Json(wb): Json<WireBlock>) -> StatusCode {
    // 0) vérif network_id + version
    if wb.network_id != st._cfg.network.network_id {
        return StatusCode::BAD_REQUEST;
    }
    // Protocol Check: on cast config u32 -> u16 si WireBlock est limité
    if u32::from(wb.protocol_version) != st._cfg.network.protocol_version {
        return StatusCode::BAD_REQUEST;
    }

    // 0b) vérif signature
    // On exige la signature SI la config le demande OU SI des champs de signature sont présents (pour éviter les fausses signatures)
    let has_sig_fields = !wb.signer_pk_hex.is_empty();

    if st._cfg.auth.require_signed_submit || has_sig_fields {
        if let Err(code) = verify_wireblock_signature(&wb) {
            st.stats.persisted_err.fetch_add(1, Ordering::Relaxed);
            return code;
        }
    }

    // 1) reste inchangé
    match st.srv.adapter_arc().persist_block(&wb).await {
        Ok(PutResult::Inserted) => {
            st.stats.persisted_ok.fetch_add(1, Ordering::Relaxed);
            let _ = st
                .srv
                .broadcast(&NetMsg::Inv {
                    ids: vec![wb.id.clone()],
                })
                .await;
            crate::metrics::BLOCKS_PERSISTED.inc(); // Existant
            crate::metrics::PMS_BLOCKS_TOTAL.inc(); // Nouveau !
            StatusCode::ACCEPTED
        }
        Ok(PutResult::AlreadyExists) => {
            st.stats.persisted_dup.fetch_add(1, Ordering::Relaxed);
            StatusCode::CONFLICT
        }
        Ok(PutResult::Rejected(reason)) => {
            tracing::warn!("❌ Block Rejected: {}", reason);
            st.stats.persisted_err.fetch_add(1, Ordering::Relaxed);
            StatusCode::BAD_REQUEST
        }
        Err(e) => {
            tracing::error!("❌ Internal Persist Error: {:#}", e);
            st.stats.persisted_err.fetch_add(1, Ordering::Relaxed);
            StatusCode::INTERNAL_SERVER_ERROR
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn verify_wireblock_signature(wb: &WireBlock) -> Result<(), StatusCode> {
    // 1) signer / signature présents ?
    if wb.signer_pk_hex.is_empty() || wb.signature_hex.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 2) reconstruire la clé publique
    let pk_bytes = <Vec<u8>>::from_hex(&wb.signer_pk_hex).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let verify_key =
        VerifyingKey::from_sec1_bytes(&pk_bytes).map_err(|_| StatusCode::UNAUTHORIZED)?;

    // 3) reconstruire la signature (Base64 DER)
    let sig_bytes = general_purpose::STANDARD
        .decode(&wb.signature_hex)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let sig = Signature::from_der(&sig_bytes).map_err(|_| StatusCode::UNAUTHORIZED)?;

    // 4) message canonique identique à celui du CLI
    let msg = canonical_wireblock_message(wb);

    // 5) vérif
    verify_key
        .verify(msg.as_bytes(), &sig)
        .map_err(|_| StatusCode::UNAUTHORIZED)
}
