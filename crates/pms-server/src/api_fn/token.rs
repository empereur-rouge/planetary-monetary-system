use crate::api::AppState;
use crate::helper::is_admin_authorized;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use pms_types_payload::TokenMetadata;
use serde::Deserialize;
use serde_json::json;

// ═══════════════════════════════════════════════════════════════════════════════
// Public endpoints (client-facing)
// ═══════════════════════════════════════════════════════════════════════════════

/// GET /v1/tokens — Liste tous les tokens enregistrés.
pub async fn list_tokens(State(state): State<AppState>) -> impl IntoResponse {
    match state.store.list_tokens() {
        Ok(tokens) => (StatusCode::OK, Json(json!({ "tokens": tokens }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to list tokens: {}", e) })),
        ),
    }
}

/// GET /v1/tokens/:asset_id — Info d'un token spécifique.
pub async fn get_token(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
) -> impl IntoResponse {
    match state.store.get_token(&asset_id) {
        Ok(Some(meta)) => (StatusCode::OK, Json(json!(meta))),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("token not found: {}", asset_id) })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to get token: {}", e) })),
        ),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Admin endpoints (coordinator only)
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct CreateTokenRequest {
    pub asset_id: String,
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    #[serde(default)]
    pub max_supply: Option<String>,
}

/// POST /admin/tokens/create — Crée un nouveau token.
///
/// Le coordinator enregistre le token dans le registre et émet un bloc TokenCreate.
pub async fn admin_create_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateTokenRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    // Validate asset_id format (alphanumeric lowercase, 1-32 chars)
    if req.asset_id.is_empty()
        || req.asset_id.len() > 32
        || !req.asset_id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "asset_id must be 1-32 chars, lowercase alphanumeric or underscore" })),
        );
    }

    // Check if token already exists
    match state.store.get_token(&req.asset_id) {
        Ok(Some(_)) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": format!("token already exists: {}", req.asset_id) })),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("registry check failed: {}", e) })),
            );
        }
        Ok(None) => {} // OK, token doesn't exist yet
    }

    // Get coordinator address as creator
    let settings = &*state.settings;
    let coordinator_pk = match &settings.validation.coordinator_public_key {
        Some(pk) => pk.clone(),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "coordinator public key not configured" })),
            );
        }
    };

    let metadata = TokenMetadata {
        asset_id: req.asset_id.clone(),
        symbol: req.symbol,
        name: req.name,
        decimals: req.decimals,
        max_supply: req.max_supply,
        creator: coordinator_pk.clone(),
        mint_authority: coordinator_pk,
    };

    // Register in the token registry (RocksDB)
    if let Err(e) = state.store.register_token(&metadata) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to register token: {}", e) })),
        );
    }

    tracing::info!("[ADMIN] Token created: {} ({})", metadata.asset_id, metadata.symbol);

    (
        StatusCode::CREATED,
        Json(json!({
            "status": "ok",
            "token": metadata
        })),
    )
}

#[derive(Debug, Deserialize)]
pub struct MintTokenRequest {
    pub asset_id: String,
    pub to: String,
    pub amount: String,
}

/// POST /admin/tokens/mint — Mint des tokens custom pour un destinataire.
///
/// Le coordinator crée un bloc Mint avec l'asset_id spécifié.
pub async fn admin_mint_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<MintTokenRequest>,
) -> impl IntoResponse {
    use pms_types::TxOutput;
    use pms_types_block::Block;
    use pms_types_payload::{PayloadEnvelope, PlainPayload};
    use pms_utils::check_pow::check_pow_leading_zero_bits;
    use pms_utils::compute_block_id;
    use pms_wallet::SignerBackend;
    use pms_wallet::signing_wire::canonical_wireblock_message;
    use pms_wire::WireBlock;
    use pms_storage::PutResult;
    use rust_decimal::Decimal;

    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }

    // Validate amount
    let amount_dec = match Decimal::from_str_exact(&req.amount) {
        Ok(d) if d > Decimal::ZERO => d,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "amount must be a positive decimal" })),
            );
        }
    };

    // Check token exists in registry
    let token_meta = match state.store.get_token(&req.asset_id) {
        Ok(Some(meta)) => meta,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("token not found: {}", req.asset_id) })),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("registry lookup failed: {}", e) })),
            );
        }
    };

    // Check max_supply if defined
    if let Some(ref max_supply_str) = token_meta.max_supply {
        if let Ok(max_supply) = Decimal::from_str_exact(max_supply_str) {
            let adapter = state.srv.adapter_arc();
            let (current_supply, _) = adapter
                .circulating_supply_by_asset(Some(&req.asset_id))
                .await;

            if current_supply + amount_dec > max_supply {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({
                        "error": "would exceed max supply",
                        "current_supply": current_supply.to_string(),
                        "requested": amount_dec.to_string(),
                        "max_supply": max_supply_str
                    })),
                );
            }
        }
    }

    // Build Mint block
    let outputs = vec![TxOutput {
        address: req.to.clone(),
        amount: amount_dec.to_string(),
        asset_id: Some(req.asset_id.clone()),
    }];

    let mint_payload = PlainPayload::Mint { outputs: outputs.clone() };

    // Resolve parent
    let parent_id = match state.srv.adapter_arc().top_tips(1).await {
        Ok(tips) if !tips.is_empty() => tips[0].clone(),
        _ => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "no tips available (DAG not initialized)" })),
            );
        }
    };

    let node_wallet = &state.node_wallet;

    let mut block = Block {
        id: String::new(),
        parents: vec![parent_id],
        payload: Some(PayloadEnvelope::Plain(mint_payload)),
        nonce: 0,
        metadata: Some(pms_types_block::BlockMetadata {
            description: Some(format!(
                "Token mint: {} {} to {}",
                amount_dec, req.asset_id, req.to
            )),
            ..Default::default()
        }),
        signer_pk: None,
        signature: None,
    };
    block.id = compute_block_id(&block.parents, &block.payload, block.nonce);

    // PoW
    let min_bits = state.srv.adapter_arc().min_pow_leading_zero_bits();
    if min_bits > 0 {
        while !check_pow_leading_zero_bits(&block.id, min_bits) {
            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
        }
    }

    // Build WireBlock & sign
    let payload_json = match serde_json::to_string(&block.payload) {
        Ok(j) => j,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("failed to serialize payload: {}", e) })),
            );
        }
    };

    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json: Some(payload_json),
        nonce: block.nonce,
        network_id: state._cfg.network.network_id.clone(),
        protocol_version: state._cfg.network.protocol_version as u16,
        signer_pk_hex: node_wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: block.metadata.clone(),
    };

    let msg = canonical_wireblock_message(&wb);
    match node_wallet.sign(&msg) {
        Ok(sig) => wb.signature_hex = sig,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("failed to sign block: {}", e) })),
            );
        }
    }

    // Persist & update UTXOs
    match state.srv.adapter_arc().persist_block(&wb).await {
        Ok(PutResult::Inserted) => {
            crate::metrics::BLOCKS_PERSISTED.with_label_values(&[&state.ledger_id]).inc();
            crate::metrics::PMS_BLOCKS_TOTAL.with_label_values(&[&state.ledger_id]).inc();
            let _ = state.srv.enqueue_broadcast(wb.id.clone()).await;

            // Update UTXO set
            let adapter = state.srv.adapter_arc();
            adapter
                .add_utxo(
                    wb.id.clone(),
                    0,
                    req.to.clone(),
                    amount_dec.to_string(),
                    Some(req.asset_id.clone()),
                )
                .await;

            tracing::info!(
                "[ADMIN] Token mint: {} {} to {}",
                amount_dec,
                req.asset_id,
                req.to
            );

            (
                StatusCode::OK,
                Json(json!({
                    "status": "ok",
                    "block_id": wb.id,
                    "asset_id": req.asset_id,
                    "amount": amount_dec.to_string(),
                    "to": req.to
                })),
            )
        }
        Ok(PutResult::AlreadyExists) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "block already exists" })),
        ),
        Ok(PutResult::Rejected(reason)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": format!("block rejected: {}", reason) })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to persist mint block: {}", e) })),
        ),
    }
}
