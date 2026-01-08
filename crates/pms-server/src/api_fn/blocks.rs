use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose;
use hex::FromHex;
use k256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use std::sync::atomic::Ordering;

use crate::api::AppState;
use pms_storage::PutResult;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::WireBlock;

use axum::response::IntoResponse;

pub async fn submit_block(
    State(st): State<AppState>,
    Json(wb): Json<WireBlock>,
) -> impl IntoResponse {
    // 0) vérif network_id + version
    if wb.network_id != st._cfg.network.network_id {
        return (
            StatusCode::BAD_REQUEST,
            format!("Invalid NetworkID: expected {}", st._cfg.network.network_id),
        )
            .into_response();
    }
    // Protocol Check
    if u32::from(wb.protocol_version) != st._cfg.network.protocol_version {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "Invalid Protocol Version: expected {}",
                st._cfg.network.protocol_version
            ),
        )
            .into_response();
    }

    // 0b) vérif signature
    let has_sig_fields = !wb.signer_pk_hex.is_empty();

    if st._cfg.auth.require_signed_submit || has_sig_fields {
        if let Err(code) = verify_wireblock_signature(&wb) {
            st.stats.persisted_err.fetch_add(1, Ordering::Relaxed);
            return (code, "Signature verification failed").into_response();
        }
    }

    // 1) reste inchangé
    match st.srv.adapter_arc().persist_block(&wb).await {
        Ok(PutResult::Inserted) => {
            st.stats.persisted_ok.fetch_add(1, Ordering::Relaxed);
            let _ = st.srv.enqueue_broadcast(wb.id.clone()).await;

            crate::metrics::BLOCKS_PERSISTED.inc();
            crate::metrics::PMS_BLOCKS_TOTAL.inc();
            StatusCode::ACCEPTED.into_response()
        }
        Ok(PutResult::AlreadyExists) => {
            st.stats.persisted_dup.fetch_add(1, Ordering::Relaxed);
            StatusCode::CONFLICT.into_response()
        }
        Ok(PutResult::Rejected(reason)) => {
            tracing::warn!("❌ Block Rejected: {}", reason);
            st.stats.persisted_err.fetch_add(1, Ordering::Relaxed);
            (StatusCode::BAD_REQUEST, reason).into_response()
        }
        Err(e) => {
            tracing::error!("❌ Internal Persist Error: {:#}", e);
            st.stats.persisted_err.fetch_add(1, Ordering::Relaxed);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
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
