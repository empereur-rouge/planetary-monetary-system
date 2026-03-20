//! API endpoints pour les NFTs.
//!
//! Permet de query l'état des NFTs (ownership, existence).

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use pms_types_nft::NftMetadata; // Used for MintNftRequest
use serde::{Deserialize, Serialize};

use crate::api::AppState;
use pms_storage::{DagStorage, NftStorage};
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_wallet::SignerBackend;
use pms_wire::WireBlock;

// ═══════════════════════════════════════════════════════════════════════════
// Helper: Coordinator-side Decryption of NFT Metadata
// ═══════════════════════════════════════════════════════════════════════════

/// Déchiffre les métadonnées d'un NFT depuis le bloc DAG.
///
/// Le coordinateur peut déchiffrer car il est dans la liste des recipients
/// lors du chiffrement (voir `mint_nft`).
///
/// # Arguments
/// * `state` - AppState contenant le store et le node_wallet
/// * `token_id` - ID du token dont on veut les métadonnées
///
/// # Returns
/// * `Some(NftMetadata)` si déchiffrement réussi
/// * `None` si le token n'existe pas, pas de block_id, ou déchiffrement échoué
pub async fn decrypt_nft_metadata_from_dag(
    state: &AppState,
    token_id: &str,
) -> Option<NftMetadata> {
    // 1. Récupérer le block_id depuis le store NFT
    let block_id = state.store.get_block_id(token_id).ok()??;

    // 2. Récupérer le bloc depuis le DAG
    let stored_block = state.store.get_block(&block_id).await.ok()??;

    // 3. Parser le payload du bloc
    let payload_json = stored_block.payload_json?;
    let envelope: PayloadEnvelope = serde_json::from_str(&payload_json).ok()?;

    // 4. Extraire le payload chiffré
    //    - Soit direct (Mint) : PayloadEnvelope::Encrypted
    //    - Soit imbriqué (Transfer) : PlainPayload::Nft(Transfer { encrypted_metadata: Some(str) })
    let encrypted = match envelope {
        PayloadEnvelope::Encrypted(enc) => enc,
        PayloadEnvelope::Plain(PlainPayload::Nft(pms_types_nft::NftAction::Transfer {
            encrypted_metadata: Some(enc_str),
            ..
        })) => {
            // Le payload est une String JSON à désérialiser
            match serde_json::from_str::<EncryptedPayload>(&enc_str) {
                Ok(enc) => enc,
                Err(e) => {
                    tracing::error!(
                        "Failed to parse encrypted_metadata in Transfer block {}: {}",
                        block_id,
                        e
                    );
                    return None;
                }
            }
        }
        _ => {
            tracing::debug!("Block {} has no compatible encrypted payload", block_id);
            return None;
        }
    };

    // 5. Déchiffrer avec la clé X25519 du coordinateur
    let x25519_sk = state.node_wallet.x25519_sk_hex()?;
    let plaintext = match encrypted.decrypt_with(&x25519_sk) {
        Ok(pt) => pt,
        Err(e) => {
            tracing::debug!("Failed to decrypt block {}: {}", block_id, e);
            return None;
        }
    };

    // 6. Désérialiser les métadonnées
    //    Le plaintext est le JSON des métadonnées (encodé lors du mint)
    let metadata: NftMetadata = serde_json::from_slice(&plaintext).ok()?;

    tracing::debug!(
        "✅ Decrypted metadata for token {} from block {}",
        token_id,
        &block_id[..16.min(block_id.len())]
    );

    Some(metadata)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NftResponse {
    /// Token ID demandé
    pub token_id: String,
    /// Propriétaire actuel (None si le token n'existe pas)
    pub owner: Option<String>,
    /// ID du bloc contenant les métadonnées chiffrées (pour récupération + déchiffrement client)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mint_block_id: Option<String>,
    /// Le token existe-t-il ?
    pub exists: bool,
}

/// Réponse pour GET /v1/wallet/{address}/nfts
#[derive(Debug, Serialize, Deserialize)]
pub struct NftsListResponse {
    /// Adresse du propriétaire
    pub owner: String,
    /// Liste des token_ids possédés
    pub token_ids: Vec<String>,
    /// Nombre total de NFTs
    pub count: usize,
}

/// GET /v1/nft/{token_id}
///
/// Query l'ownership d'un NFT par son token_id.
///
/// # Réponses
/// - 200 OK : Token trouvé avec owner
/// - 404 Not Found : Token inexistant (exists: false)
/// - 500 Internal Server Error : Erreur de lecture store
pub async fn get_nft(
    State(state): State<AppState>,
    Path(token_id): Path<String>,
) -> impl IntoResponse {
    use pms_storage::NftStorage;

    // Accède au store directement depuis AppState
    let store = &state.store;

    match store.get_owner(&token_id) {
        Ok(Some(owner)) => {
            // Token existe avec un owner
            // On récupère le block_id qui contient les métadonnées chiffrées
            let mint_block_id = store.get_block_id(&token_id).unwrap_or(None);

            let response = NftResponse {
                token_id,
                owner: Some(owner),
                mint_block_id,
                exists: true,
            };
            (StatusCode::OK, Json(response))
        }
        Ok(None) => {
            // Token n'existe pas
            let response = NftResponse {
                token_id,
                owner: None,
                mint_block_id: None,
                exists: false,
            };
            (StatusCode::NOT_FOUND, Json(response))
        }
        Err(e) => {
            // Erreur interne
            tracing::error!("NFT get_owner error: {}", e);
            let response = NftResponse {
                token_id,
                owner: None,
                mint_block_id: None,
                exists: false,
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(response))
        }
    }
}

/// GET /v1/wallet/{address}/nfts
///
/// Récupère la liste des NFTs appartenant à une adresse.
///
/// # Réponses
/// - 200 OK : Liste des token_ids (peut être vide)
/// - 500 Internal Server Error : Erreur de lecture store
pub async fn get_nfts_by_owner(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    use pms_storage::NftStorage;

    let store = &state.store;

    match store.get_by_owner(&address) {
        Ok(token_ids) => {
            let count = token_ids.len();
            let response = NftsListResponse {
                owner: address,
                token_ids,
                count,
            };
            (StatusCode::OK, Json(response))
        }
        Err(e) => {
            tracing::error!("NFT get_by_owner error: {}", e);
            let response = NftsListResponse {
                owner: address,
                token_ids: Vec::new(),
                count: 0,
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(response))
        }
    }
}

/// Request pour POST /v1/nft/mint
/// Le client fournit les données (Metadata, Token ID) et le Serveur (Coordinateur) signe et mint.
#[derive(Debug, Deserialize)]
pub struct MintNftRequest {
    /// Token ID généré par le client (doit être unique)
    pub token_id: String,
    /// Adresse du propriétaire
    pub owner_address: String,
    /// Clé publique X25519 du propriétaire (hex) pour le chiffrement
    pub owner_x25519_pubkey: String,
    /// Métadonnées du NFT
    pub metadata: NftMetadata,
}

/// Handler générique pour minter un NFT (Signé par le Coordinateur)
pub async fn mint_nft(
    State(state): State<AppState>,
    Json(req): Json<MintNftRequest>,
) -> impl IntoResponse {
    // 0. Gas pool check (custom ledgers only)
    if let Err(e) = crate::api_fn::tx_helpers::try_consume_gas(&state) {
        return (StatusCode::PAYMENT_REQUIRED, e).into_response();
    }

    // 1. Vérif basique
    if req.token_id.len() != 64 {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid token_id length (must be 64 hex chars)",
        )
            .into_response();
    }

    // 1b. Compute NFT mint fee (if configured and not exempt)
    let nft_fee = {
        let nft_type = req.metadata.nft_type.as_deref();
        if crate::api_fn::tx_helpers::is_nft_type_fee_exempt(
            &state.store,
            nft_type,
            Some(&state.effective_fees),
        ) {
            rust_decimal::Decimal::ZERO
        } else {
            crate::api_fn::tx_helpers::load_nft_mint_fee(&state.store, Some(&state.effective_fees))
                .unwrap_or(rust_decimal::Decimal::ZERO)
        }
    };

    // 1c. Compute storage fee (if configured) — charged on metadata payload size
    let metadata_bytes = serde_json::to_vec(&req.metadata)
        .map(|b| b.len())
        .unwrap_or(0);
    let storage_fee = crate::api_fn::tx_helpers::load_storage_fee_per_kb(
        &state.store,
        Some(&state.effective_fees),
    )
    .map(|fee_per_kb| pms_economics::storage_fee::calculate_storage_fee(metadata_bytes, fee_per_kb))
    .unwrap_or(rust_decimal::Decimal::ZERO);
    let nft_fee = nft_fee + storage_fee;

    // 2. Chiffrer les métadonnées (Privacy)
    let coord_x25519 = state.node_wallet.x25519_pub_hex();
    let recipients = vec![req.owner_x25519_pubkey.clone(), coord_x25519.to_string()];

    let plaintext = match serde_json::to_vec(&req.metadata) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Json error: {e}"),
            )
                .into_response();
        }
    };

    let encrypted_payload =
        match EncryptedPayload::encrypt_for(&plaintext, &recipients, plaintext.len() as u32) {
            Ok(ep) => ep,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Encrypt error: {e}"),
                )
                    .into_response();
            }
        };

    // 3. Construire le bloc
    let parents: Vec<String> = state
        .srv
        .adapter_arc()
        .top_tips(2)
        .await
        .unwrap_or_default();

    // Si pas de parents, on ne peut pas minter au dessus de rien (sauf si on est le tout premier bloc, mais edge case)
    if parents.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "No tips available to attach block",
        )
            .into_response();
    }

    let payload = PayloadEnvelope::Encrypted(encrypted_payload);
    let payload_json = match serde_json::to_string(&payload) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Payload serialization error: {e}"),
            )
                .into_response();
        }
    };

    let nonce = 0;

    // Block ID (hash)
    let block_id = pms_utils::compute_block_id(&parents, &Some(payload), nonce);

    // 5. Construire WireBlock non signé
    let mut wire_block = WireBlock {
        id: block_id.clone(),
        parents,
        payload_json: Some(payload_json),
        nonce,
        network_id: state._cfg.network.network_id.clone(),
        protocol_version: state._cfg.network.protocol_version as u16,
        signer_pk_hex: state.node_wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: None,
    };

    // 6. Signer le message canonique
    // Il est CRITIQUE de signer le même message que celui vérifié par blocks.rs (canonical_wireblock_message)
    // et NON juste le block_id.
    let msg_to_sign = pms_wallet::signing_wire::canonical_wireblock_message(&wire_block);
    let signature_hex = match state.node_wallet.sign(&msg_to_sign) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Sign error: {e:?}"),
            )
                .into_response();
        }
    };
    wire_block.signature_hex = signature_hex;

    // 6. Soumettre (Persist + Broadcast)
    match state.srv.adapter_arc().persist_block(&wire_block).await {
        Ok(pms_storage::PutResult::Inserted) => {
            let _ = state.srv.enqueue_broadcast(wire_block.id.clone()).await;

            crate::metrics::BLOCKS_PERSISTED
                .with_label_values(&[&state.ledger_id])
                .inc();

            // Index activity for encrypted NFT payload (owner address + Nft category).
            // Pre-computed items are not available here (plain payload already encrypted);
            // they will be computed from the block at read time (fallback path).
            {
                let addrs = vec![req.owner_address.clone()];
                let typed = vec![(
                    req.owner_address.clone(),
                    pms_storage::helpers::ActivityCategory::Nft,
                )];
                if let Err(e) = state
                    .store
                    .write_addr_activity_entries_with_categories(&block_id, &addrs, &typed, None)
                {
                    tracing::warn!("addr_activity index for encrypted NFT block: {e}");
                }
            }

            {
                use pms_storage::NftStorage;

                // Privacy-first: on ne stocke PAS les métadonnées en clair.
                // On stocke seulement (token_id, owner, block_id).
                // Les métadonnées sont chiffrées dans le bloc du DAG.
                if let Err(e) = state.store.apply_mint(
                    &req.token_id,
                    &req.owner_address,
                    &block_id, // Référence au bloc contenant les métadonnées chiffrées
                ) {
                    // Le bloc est déjà persisté, on log l'erreur mais on ne fail pas
                    // car le bloc est dans le DAG (source de vérité)
                    tracing::error!("❌ NFT store apply_mint failed after block inserted: {}", e);
                } else {
                    tracing::info!(
                        "✅ NFT {} minted to {} (store updated, metadata in block {})",
                        req.token_id,
                        req.owner_address,
                        &block_id[..16]
                    );
                }
            }

            // Accumulate NFT mint fee in pool for periodic distribution
            let reward_block_id: Option<String> = None;
            if nft_fee > rust_decimal::Decimal::ZERO {
                crate::api_fn::tx_helpers::accumulate_tx_fee(&state, nft_fee).await;
                tracing::info!(
                    "NFT mint fee {} PMS accumulated in pool for token {}",
                    nft_fee,
                    &req.token_id[..16.min(req.token_id.len())]
                );
            }

            let response = serde_json::json!({
                "status": "inserted",
                "block_id": block_id,
                "nft_mint_fee": nft_fee.to_string(),
                "reward_block_id": reward_block_id
            });
            (StatusCode::OK, Json(response)).into_response()
        }
        Ok(pms_storage::PutResult::AlreadyExists) => {
            (StatusCode::CONFLICT, "Block already exists").into_response()
        }
        Ok(pms_storage::PutResult::Rejected(r)) => {
            (StatusCode::BAD_REQUEST, format!("Rejected: {r}")).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Storage error: {e}"),
        )
            .into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// CONTRACT ENGINE — Post-burn event emission
// ═══════════════════════════════════════════════════════════════════════════

/// Émet un événement `NftBurnProcessed` sur le **main EventBus** pour déclencher
/// l'évaluation des contrats déclaratifs par le listener dans `pms-contracts`.
///
/// Utilise `state.contract_event_bus` (le bus du main adapter) et NON
/// `state.srv.adapter_arc().event_bus()` qui retourne le bus per-ledger.
/// Le `ContractListener` est abonné uniquement au bus main — les burns sur
/// les custom ledgers (eden, etc.) doivent émettre sur ce même bus.
///
/// **IMPORTANT**: Les metadata doivent être récupérées AVANT `apply_action()`
/// car `apply_action` supprime le `block_id` du NFT, rendant les métadonnées
/// irrécupérables depuis le DAG.
fn emit_nft_burn_processed(
    state: &AppState,
    block_id: &str,
    token_ids: &[String],
    burner_address: &str,
    pre_fetched_metadata: Option<pms_types_nft::NftMetadata>,
) {
    if let Some(bus) = &state.contract_event_bus {
        bus.emit(pms_event::PmsEvent::nft_burn_processed(
            block_id.to_string(),
            state.ledger_id.clone(),
            burner_address.to_string(),
            token_ids.to_vec(),
            pre_fetched_metadata,
        ));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// BURN NFT ENDPOINT
// ═══════════════════════════════════════════════════════════════════════════

/// Réponse pour POST /v1/nft/burn
#[derive(Debug, Serialize, Deserialize)]
pub struct BurnNftResponse {
    pub status: String,
    pub block_id: String,
    pub token_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_ids: Option<Vec<String>>,
}

/// POST /v1/nft/burn
///
/// Endpoint pour burn un NFT. Le client doit fournir un `WireBlock`
/// pré-signé contenant un payload `NftAction::Burn` ou `NftAction::BatchBurn`.
///
/// Le client doit signer car la validation NFT exige que le `signer`
/// du bloc soit égal au `burner`.
pub async fn burn_nft(
    State(state): State<AppState>,
    Json(wb): Json<WireBlock>,
) -> impl IntoResponse {
    // Gas pool check (custom ledgers only)
    if let Err(e) = crate::api_fn::tx_helpers::try_consume_gas(&state) {
        return (StatusCode::PAYMENT_REQUIRED, e).into_response();
    }

    use pms_storage::NftStorage;

    let payload_json = match &wb.payload_json {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Missing payload_json in WireBlock"
                })),
            )
                .into_response();
        }
    };

    let envelope: PayloadEnvelope = match serde_json::from_str(payload_json) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("Invalid PayloadEnvelope: {}", e)
                })),
            )
                .into_response();
        }
    };

    let nft_action = match envelope {
        PayloadEnvelope::Plain(pms_types_payload::PlainPayload::Nft(action)) => action,
        PayloadEnvelope::Plain(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Payload is not an NftAction (expected PlainPayload::Nft)"
                })),
            )
                .into_response();
        }
        PayloadEnvelope::Encrypted(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Burn action should not be encrypted"
                })),
            )
                .into_response();
        }
    };

    let (token_ids_to_process, burner) = match &nft_action {
        pms_types_nft::NftAction::Burn { token_id, burner } => {
            (vec![token_id.clone()], burner.clone())
        }
        pms_types_nft::NftAction::BatchBurn { token_ids, burner } => {
            (token_ids.clone(), burner.clone())
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Expected NftAction::Burn or BatchBurn, got different action type"
                })),
            )
                .into_response();
        }
    };

    // Vérifier que TOUS les NFTs existent encore
    {
        for tid in &token_ids_to_process {
            match state.store.get_owner(tid) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    tracing::warn!(
                        "Batch burn failure: Token {} already burned or missing",
                        &tid[..16.min(tid.len())]
                    );
                    return (
                        StatusCode::CONFLICT,
                        Json(serde_json::json!({
                            "error": format!("NFT already burned or does not exist: {}", tid),
                            "token_id": tid
                        })),
                    )
                        .into_response();
                }
                Err(e) => {
                    tracing::error!("Failed to check NFT existence for {}: {}", tid, e);
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "error": format!("Failed to check NFT existence: {}", e)
                        })),
                    )
                        .into_response();
                }
            }
        }
    }

    // Pre-fetch metadata BEFORE burn for contract evaluation
    let pre_metadata = if let Some(first_tid) = token_ids_to_process.first() {
        decrypt_nft_metadata_from_dag(&state, first_tid).await
    } else {
        None
    };

    // Persister le bloc dans le DAG
    let block_id = wb.id.clone();

    match state.srv.adapter_arc().persist_block(&wb).await {
        Ok(pms_storage::PutResult::Inserted) => {
            let _ = state.srv.enqueue_broadcast(block_id.clone()).await;
            crate::metrics::BLOCKS_PERSISTED
                .with_label_values(&[&state.ledger_id])
                .inc();

            if let Err(e) = state.store.apply_action(&nft_action) {
                tracing::error!("Failed to apply BURN action to NFT store: {}", e);
            } else {
                tracing::info!(
                    "{} NFTs marked as burned by {}",
                    token_ids_to_process.len(),
                    burner
                );
            }

            // Emit NftBurnProcessed event for contract listener
            emit_nft_burn_processed(
                &state,
                &block_id,
                &token_ids_to_process,
                &burner,
                pre_metadata,
            );

            let response_token_id = if token_ids_to_process.len() == 1 {
                token_ids_to_process[0].clone()
            } else {
                "batch".to_string()
            };

            let response = BurnNftResponse {
                status: "burned".to_string(),
                block_id,
                token_id: response_token_id,
                token_ids: Some(token_ids_to_process),
            };

            (StatusCode::OK, Json(response)).into_response()
        }
        Ok(pms_storage::PutResult::AlreadyExists) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Block already exists",
                "block_id": block_id
            })),
        )
            .into_response(),
        Ok(pms_storage::PutResult::Rejected(reason)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("Block rejected: {}", reason),
                "block_id": block_id
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Storage error: {}", e)
            })),
        )
            .into_response(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PREPARE TRANSFER (Coordinator re-encryption)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PrepareTransferRequest {
    pub token_id: String,
    pub to_address: String,
    pub from_address: String,
    pub new_owner_x25519_pubkey: String,
}

#[derive(Debug, Serialize)]
pub struct PrepareTransferResponse {
    /// Action préparée avec métadonnées re-chiffrées, prête à être signée
    pub action: pms_types_nft::NftAction,
}

/// POST /v1/nft/transfer/prepare
///
/// Prépare une action de transfert avec re-chiffrement des métadonnées.
/// Le coordinateur :
/// 1. Déchiffre les métadonnées actuelles (car il est destinataire)
/// 2. Re-chiffre pour le nouveau propriétaire + coordinateur
/// 3. Retourne l'action Transfer complète pour signature par le client
pub async fn prepare_nft_transfer(
    State(state): State<AppState>,
    Json(req): Json<PrepareTransferRequest>,
) -> impl IntoResponse {
    // 1. Déchiffrer les métadonnées
    let metadata = match decrypt_nft_metadata_from_dag(&state, &req.token_id).await {
        Some(m) => m,
        None => {
            return (
                StatusCode::NOT_FOUND,
                "Metadata not found or decryption failed",
            )
                .into_response();
        }
    };

    // 2. Chiffrer pour le nouveau propriétaire + coordinateur
    let coord_x25519 = state.node_wallet.x25519_pub_hex();
    let recipients = vec![
        req.new_owner_x25519_pubkey.clone(),
        coord_x25519.to_string(),
    ];

    let plaintext = match serde_json::to_vec(&metadata) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Json error: {e}"),
            )
                .into_response();
        }
    };

    let encrypted_payload =
        match EncryptedPayload::encrypt_for(&plaintext, &recipients, plaintext.len() as u32) {
            Ok(ep) => ep,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Encrypt error: {e}"),
                )
                    .into_response();
            }
        };

    // Sérialiser le payload chiffré en string JSON pour l'inclure dans l'action
    let encrypted_metadata_json = match serde_json::to_string(&encrypted_payload) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Payload serialize error: {e}"),
            )
                .into_response();
        }
    };

    // 3. Construire l'action Transfer
    let action = pms_types_nft::NftAction::Transfer {
        token_id: req.token_id,
        from: req.from_address,
        to: req.to_address,
        new_owner_x25519_pubkey: Some(req.new_owner_x25519_pubkey),
        encrypted_metadata: Some(encrypted_metadata_json),
    };

    (StatusCode::OK, Json(PrepareTransferResponse { action })).into_response()
}

// ═══════════════════════════════════════════════════════════════════════════
// BURN NFT SIMPLE (like send-simple: takes private_key + token_id)
// ═══════════════════════════════════════════════════════════════════════════

/// Request pour POST /v1/nft/burn-simple
#[derive(Debug, Deserialize)]
pub struct BurnNftSimpleRequest {
    /// Clé privée base64 du propriétaire
    pub private_key_b64: String,
    /// ID du token à brûler
    pub token_id: String,
}

/// Request pour POST /v1/nft/burn-batch-simple
#[derive(Debug, Deserialize)]
pub struct BurnNftBatchSimpleRequest {
    /// Clé privée base64 du propriétaire
    pub private_key_b64: String,
    /// IDs des tokens à brûler
    pub token_ids: Vec<String>,
}

/// POST /v1/nft/burn-simple
///
/// Endpoint simplifié pour burn un NFT. Le serveur construit et signe
/// le WireBlock à partir de la clé privée fournie (comme send-simple).
pub async fn burn_nft_simple(
    State(state): State<AppState>,
    Json(req): Json<BurnNftSimpleRequest>,
) -> impl IntoResponse {
    // Gas pool check (custom ledgers only)
    if let Err(e) = crate::api_fn::tx_helpers::try_consume_gas(&state) {
        return (StatusCode::PAYMENT_REQUIRED, e).into_response();
    }

    use crate::api_fn::tx_helpers;
    use crate::api_fn::wallet_factory::wallet_from_b64;
    use pms_types_nft::NftAction;

    // 1. Reconstruire le wallet depuis la clé privée
    let sender_wallet = match wallet_from_b64(&req.private_key_b64) {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("invalid wallet key: {e}") })),
            )
                .into_response();
        }
    };

    let burner_pk = sender_wallet.encoded_public_key();
    let burner_addr = sender_wallet.get_address(&state.settings.address.hrp);

    // 2. Vérifier ownership (owner stored as bech32 address)
    {
        match state.store.get_owner(&req.token_id) {
            Ok(Some(owner)) if owner == burner_addr || owner == burner_pk => {} // OK
            Ok(Some(owner)) => {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": format!("Not the owner: owner={}, burner={}", owner, burner_addr)
                    })),
                )
                    .into_response();
            }
            Ok(None) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": "NFT not found or already burned"
                    })),
                )
                    .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Failed to check ownership: {e}")
                    })),
                )
                    .into_response();
            }
        }
    }

    // 3. Construire le payload NftAction::Burn
    // burner must be bech32 address (same format as stored owner)
    let nft_action = NftAction::Burn {
        token_id: req.token_id.clone(),
        burner: burner_addr.clone(),
    };
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Nft(
        nft_action.clone(),
    )));

    // 4. Get parents
    let settings = &*state.settings;
    let parents = match tx_helpers::get_block_parents(&state.store, settings).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    };

    // 5. Forge block + sign avec le node_wallet (coordinator, single-writer mode)
    let adapter = state.srv.adapter_arc();
    let wb = match tx_helpers::forge_and_sign_block(
        payload,
        parents,
        &adapter,
        &state.node_wallet,
        settings,
        Some("NFT burn-simple"),
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    };

    // 5b. Pre-fetch NFT metadata BEFORE apply_action (which deletes block_id)
    let pre_metadata = decrypt_nft_metadata_from_dag(&state, &req.token_id).await;

    // 6. Persist + broadcast
    let block_id = wb.id.clone();
    let token_id_for_response = req.token_id.clone();
    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(pms_storage::PutResult::Inserted) => {
            // Apply NFT action to store
            if let Err(e) = state.store.apply_action(&nft_action) {
                tracing::error!("Failed to apply burn action to NFT store: {}", e);
            } else {
                tracing::info!(
                    "NFT {} burned by {} via burn-simple (block {})",
                    &req.token_id[..16.min(req.token_id.len())],
                    &burner_pk[..20.min(burner_pk.len())],
                    &block_id[..16.min(block_id.len())]
                );
            }

            // Emit NftBurnProcessed event for contract listener
            emit_nft_burn_processed(
                &state,
                &block_id,
                &[req.token_id],
                &burner_addr,
                pre_metadata,
            );

            (
                StatusCode::OK,
                Json(serde_json::json!(BurnNftResponse {
                    status: "burned".to_string(),
                    block_id,
                    token_id: token_id_for_response,
                    token_ids: None,
                })),
            )
                .into_response()
        }
        Ok(pms_storage::PutResult::AlreadyExists) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "block already exists" })),
        )
            .into_response(),
        Ok(pms_storage::PutResult::Rejected(reason)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("rejected: {reason}") })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// BURN NFT BATCH SIMPLE (burn multiple NFTs in one block)
// ═══════════════════════════════════════════════════════════════════════════

/// POST /v1/nft/burn-batch-simple
///
/// Burn multiple NFTs in a single block. The server constructs and signs
/// the block on behalf of the owner (coordinator mode, like burn-simple).
pub async fn burn_nft_batch_simple(
    State(state): State<AppState>,
    Json(req): Json<BurnNftBatchSimpleRequest>,
) -> impl IntoResponse {
    // Gas pool check (custom ledgers only)
    if let Err(e) = crate::api_fn::tx_helpers::try_consume_gas(&state) {
        return (StatusCode::PAYMENT_REQUIRED, e).into_response();
    }

    use crate::api_fn::tx_helpers;
    use crate::api_fn::wallet_factory::wallet_from_b64;
    use pms_types_nft::NftAction;

    if req.token_ids.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "token_ids must not be empty" })),
        )
            .into_response();
    }

    // 1. Reconstruct wallet from private key
    let sender_wallet = match wallet_from_b64(&req.private_key_b64) {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("invalid wallet key: {e}") })),
            )
                .into_response();
        }
    };

    let burner_pk = sender_wallet.encoded_public_key();
    let burner_addr = sender_wallet.get_address(&state.settings.address.hrp);

    // 2. Verify ownership of ALL tokens
    for token_id in &req.token_ids {
        match state.store.get_owner(token_id) {
            Ok(Some(owner)) if owner == burner_addr || owner == burner_pk => {} // OK
            Ok(Some(owner)) => {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": format!("Not the owner of {}: owner={}, burner={}", token_id, owner, burner_addr)
                    })),
                )
                    .into_response();
            }
            Ok(None) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": format!("NFT {} not found or already burned", token_id)
                    })),
                )
                    .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Failed to check ownership of {}: {e}", token_id)
                    })),
                )
                    .into_response();
            }
        }
    }

    // 3. Build NftAction::BatchBurn payload
    let nft_action = NftAction::BatchBurn {
        token_ids: req.token_ids.clone(),
        burner: burner_addr.clone(),
    };
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Nft(
        nft_action.clone(),
    )));

    // 4. Get parents
    let settings = &*state.settings;
    let parents = match tx_helpers::get_block_parents(&state.store, settings).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    };

    // 5. Forge block + sign with node_wallet (coordinator, single-writer mode)
    let adapter = state.srv.adapter_arc();
    let wb = match tx_helpers::forge_and_sign_block(
        payload,
        parents,
        &adapter,
        &state.node_wallet,
        settings,
        Some("NFT burn-batch-simple"),
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    };

    // 5b. Pre-fetch metadata from first token BEFORE burn (apply_action deletes block_id)
    let pre_metadata = if let Some(first_id) = req.token_ids.first() {
        decrypt_nft_metadata_from_dag(&state, first_id).await
    } else {
        None
    };

    // 6. Persist + broadcast
    let block_id = wb.id.clone();
    let count = req.token_ids.len();
    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(pms_storage::PutResult::Inserted) => {
            // Apply NFT action to store
            if let Err(e) = state.store.apply_action(&nft_action) {
                tracing::error!("Failed to apply batch burn action to NFT store: {}", e);
            } else {
                tracing::info!(
                    "Batch burned {} NFTs by {} via burn-batch-simple (block {})",
                    count,
                    &burner_addr[..20.min(burner_addr.len())],
                    &block_id[..16.min(block_id.len())]
                );
            }

            // Emit NftBurnProcessed event for contract listener
            emit_nft_burn_processed(
                &state,
                &block_id,
                &req.token_ids,
                &burner_addr,
                pre_metadata,
            );

            (
                StatusCode::OK,
                Json(serde_json::json!(BurnNftResponse {
                    status: "burned".to_string(),
                    block_id,
                    token_id: req.token_ids.first().cloned().unwrap_or_default(),
                    token_ids: Some(req.token_ids),
                })),
            )
                .into_response()
        }
        Ok(pms_storage::PutResult::AlreadyExists) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "block already exists" })),
        )
            .into_response(),
        Ok(pms_storage::PutResult::Rejected(reason)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("rejected: {reason}") })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e })),
        )
            .into_response(),
    }
}
