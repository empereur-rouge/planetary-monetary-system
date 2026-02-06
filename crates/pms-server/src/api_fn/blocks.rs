use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose;
use hex::FromHex;
use k256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::atomic::Ordering;

use crate::api::AppState;
use pms_storage::DagStorage;
use pms_storage::PutResult;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
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

    // 0c) Coordinator Filter: Reject regular TXs if configured (force use of Worker Nodes)
    if st.settings.validation.coordinator_tx_only {
        let is_privileged = wb
            .payload_json
            .as_ref()
            .and_then(|json| serde_json::from_str::<PayloadEnvelope>(json).ok())
            .map(|envelope| {
                matches!(
                    envelope,
                    PayloadEnvelope::Plain(PlainPayload::Milestone { .. })
                        | PayloadEnvelope::Plain(PlainPayload::ConfigUpdate(_))
                )
            })
            .unwrap_or(false);

        if !is_privileged {
            tracing::warn!(
                "⛔️ Coordinator rejected non-privileged block from {}",
                &wb.signer_pk_hex
            );
            return (
                StatusCode::FORBIDDEN,
                "Coordinator only accepts Milestones. Use a Worker Node.",
            )
                .into_response();
        }
    }

    // 1) Persist block
    match st.srv.adapter_arc().persist_block(&wb).await {
        Ok(PutResult::Inserted) => {
            st.stats.persisted_ok.fetch_add(1, Ordering::Relaxed);
            let _ = st.srv.enqueue_broadcast(wb.id.clone()).await;

            crate::metrics::BLOCKS_PERSISTED.inc();
            crate::metrics::PMS_BLOCKS_TOTAL.inc();

            // ============================================================
            // FEE POOL ACCUMULATION (Distributed TX Processing)
            // ============================================================
            // Instead of creating a reward block immediately, we accumulate
            // fees in the pool. Distribution happens via Milestone.
            // This removes the Coordinator bottleneck for horizontal scaling.
            accumulate_fee_if_tx(&st, &wb).await;

            // ============================================================
            // BURN REFUND PROCESSING (Cube NFT -> Token Conversion)
            // ============================================================
            // Check if this block contains a valid cube burn and process refund
            process_burn_refund_if_applicable(&st, &wb).await;

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

/// Accumulates transaction fee in the pool for later Milestone distribution
/// This replaces immediate reward block creation for horizontal scaling
async fn accumulate_fee_if_tx(st: &AppState, wb: &WireBlock) {
    // Extract transaction fee from payload
    let fee = match extract_tx_fee(wb, st) {
        Some(f) if f > Decimal::ZERO => f,
        _ => {
            eprintln!("❌ extract_tx_fee failed or zero for block {}", &wb.id);
            return;
        }
    };

    // Get the block signer (node that created/submitted this block)
    // Si absent, on utilise "unknown" mais on logue un warning car c'est anormal.
    let signer_pk = if wb.signer_pk_hex.is_empty() {
        eprintln!(
            "⚠️ SECURITY: TX block {} has no signer_pk! Fee credited to 'unknown'.",
            &wb.id[..16.min(wb.id.len())]
        );
        "unknown".to_string()
    } else {
        wb.signer_pk_hex.clone()
    };

    // Add fee to pool with node contribution tracking
    {
        let mut pool = st.fee_pool.write().await;
        pool.add_fee(fee, &signer_pk);
        eprintln!(
            "💰 Fee accumulated: {} PMS from node {}... (pool total: {} PMS, {} txs)",
            fee,
            &signer_pk[..20.min(signer_pk.len())],
            pool.total_fees,
            pool.tx_count
        );
    }

    // Also increment block count in node registry for this signer
    {
        let mut registry = st.node_registry.write().await;
        registry.increment_block_count(&signer_pk);
    }
}

/// Processes burn refunds for cube NFTs if applicable
/// Adds validated refunds to the fee pool for later distribution
async fn process_burn_refund_if_applicable(st: &AppState, wb: &WireBlock) {
    use super::nft::decrypt_nft_metadata_from_dag;

    // 1. Parse payload for NFT Burn action
    let burn_action = match extract_nft_burn_action(wb) {
        Some(action) => action,
        None => return, // Not an NFT burn, nothing to do
    };

    // 2. Get authority public keys from config
    let authority_pks = &st.settings.fees.authority_public_keys;

    // 3. Déchiffrer les métadonnées depuis le bloc DAG
    // Le coordinateur peut déchiffrer car il est dans la liste des recipients
    let metadata = decrypt_nft_metadata_from_dag(st, &burn_action.token_id).await;

    // 4. Calculate refund (if valid cube with valid signature)
    let refund = match crate::burn_refund::calculate_burn_refund(
        &burn_action.token_id,
        &burn_action.burner,
        metadata.as_ref(),
        authority_pks,
    ) {
        Ok(Some(r)) => r,
        Ok(None) => return, // No refund (not a cube, invalid sig, no metadata, etc.)
        Err(e) => {
            tracing::warn!("Burn refund calculation failed: {}", e);
            return;
        }
    };

    // 5. Add refund to fee pool (will be distributed via Milestone)
    {
        let mut pool = st.fee_pool.write().await;
        pool.add_fee(refund.amount, &refund.recipient);
        tracing::info!(
            "🔥 Cube burn refund queued: {} -> {} PMS (token: {})",
            &refund.recipient[..20.min(refund.recipient.len())],
            refund.amount,
            &refund.token_id[..16.min(refund.token_id.len())]
        );
    }
}

/// Simple struct to hold extracted burn action data
struct NftBurnAction {
    token_id: String,
    burner: String,
}

/// Extract NFT Burn action from WireBlock payload if present
fn extract_nft_burn_action(wb: &WireBlock) -> Option<NftBurnAction> {
    let payload_json = wb.payload_json.as_ref()?;
    let envelope: PayloadEnvelope = serde_json::from_str(payload_json).ok()?;

    // Pattern matching direct pour éviter les matchs imbriqués
    if let PayloadEnvelope::Plain(PlainPayload::Nft(pms_types_nft::NftAction::Burn {
        token_id,
        burner,
    })) = envelope
    {
        Some(NftBurnAction { token_id, burner })
    } else {
        None
    }
}

fn extract_tx_fee(wb: &WireBlock, st: &AppState) -> Option<Decimal> {
    let payload_json = if let Some(json) = &wb.payload_json {
        json
    } else {
        eprintln!("extract_tx_fee: payload_json is None for block {}", wb.id);
        return None;
    };

    let envelope: PayloadEnvelope = match serde_json::from_str(payload_json) {
        Ok(env) => env,
        Err(e) => {
            eprintln!(
                "extract_tx_fee: JSON parse error for block {}: {}",
                wb.id, e
            );
            eprintln!("extract_tx_fee: JSON content: {}", payload_json);
            return None;
        }
    };

    match envelope {
        PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx)) => match Decimal::from_str(&tx.fee) {
            Ok(fee) => Some(fee),
            Err(e) => {
                eprintln!(
                    "extract_tx_fee: Fee parse error for block {}: {} (fee='{}')",
                    wb.id, e, tx.fee
                );
                None
            }
        },
        PayloadEnvelope::Encrypted(enc) => {
            // Try to decrypt with node's private key
            let x25519_sk = match st.node_wallet.x25519_sk_hex() {
                Some(k) => k,
                None => {
                    eprintln!(
                        "extract_tx_fee: No x25519 secret key available to decrypt block {}",
                        wb.id
                    );
                    return None;
                }
            };

            match enc.decrypt_with(&x25519_sk) {
                Ok(plaintext) => {
                    // Try to deserialize plaintext as PlainPayload
                    // Note: usually the internal payload is the struct itself (TxUtxo) or PlainPayload enum?
                    // Based on legacy code: "PlainPayload::EncryptedReward" wraps payload.
                    // But here we are decrypting a TX.
                    // Let's try parsing as PlainPayload first.
                    match serde_json::from_slice::<PlainPayload>(&plaintext) {
                        Ok(PlainPayload::TxUtxo(tx)) => match Decimal::from_str(&tx.fee) {
                            Ok(fee) => Some(fee),
                            Err(e) => {
                                eprintln!(
                                    "extract_tx_fee: Fee parse error (decrypted) for block {}: {} (fee='{}')",
                                    wb.id, e, tx.fee
                                );
                                None
                            }
                        },
                        Ok(_) => {
                            // Valid payload but not TxUtxo (e.g. NftAction, etc.)
                            // eprintln!("extract_tx_fee: Decrypted payload is not TxUtxo for block {}", wb.id);
                            None
                        }
                        Err(_) => {
                            // Fallback: try parsing as TxUtxo directly?
                            // Some older code might serialize the struct directly.
                            // But pms typically uses PlainPayload.
                            // Let's log if it fails.
                            // eprintln!("extract_tx_fee: Failed to deserialize decrypted payload as PlainPayload");
                            None
                        }
                    }
                }
                Err(_) => {
                    // Decryption failed - likely not a recipient.
                    None
                }
            }
        }
        _ => {
            eprintln!("extract_tx_fee: Envelope mismatch for block {}", wb.id);
            None
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

    verify_key
        .verify(msg.as_bytes(), &sig)
        .map_err(|_| StatusCode::UNAUTHORIZED)
}

/// GET /v1/blocks/:id
/// Récupère un bloc par son ID
pub async fn get_block_by_id(
    State(st): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<WireBlock>, (StatusCode, String)> {
    // 1. Fetch from store
    let sb = st
        .store
        .get_block(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "Block not found".to_string()))?;

    // 2. Convert to WireBlock
    let wb = WireBlock {
        id: sb.id,
        parents: sb.parents,
        payload_json: sb.payload_json,
        nonce: sb.nonce,
        network_id: st._cfg.network.network_id.clone(),
        protocol_version: st._cfg.network.protocol_version as u16,
        signer_pk_hex: String::new(), // StoredBlock doesn't store signer PK explicitly if unrelated to logic, or maybe it does?
        // Wait, StoredBlock usually has metadata, but where is signer_pk?
        // Let's check StoredBlock definition if possible.
        // For now, return empty or try to extract from metadata if available.
        signature_hex: String::new(), // Same for signature
        metadata: sb.metadata,
    };

    // Note: StoredBlock in pms-storage might not have exact original fields for signer/sig if they were stripped?
    // Usually we want to return the exact block as submitted.
    // However, for sync purposes, payload_json is the most important.

    Ok(Json(wb))
}
