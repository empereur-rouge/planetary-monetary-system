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
use pms_types::TxOutput;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::api::AppState;
use pms_storage::{DagStorage, NftStorage, PutResult};
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_wallet::SignerBackend;
use pms_wire::WireBlock;

// ═══════════════════════════════════════════════════════════════════════════
// Helper: Create Refund UTXO Block
// ═══════════════════════════════════════════════════════════════════════════

/// Creates a transaction block that mints refund tokens to a recipient.
/// This is used to immediately create UTXOs for burn refunds.
///
/// # Arguments
/// * `state` - AppState containing the node wallet and server
/// * `recipient` - Address to receive the refund
/// * `amount` - Refund amount in PMS
/// * `parent_block_id` - Parent block (usually the burn block)
///
/// # Returns
/// * `Ok(block_id)` - The ID of the created refund block
/// * `Err(e)` - Error if block creation or submission failed
async fn create_refund_utxo_block(
    state: &AppState,
    recipient: &str,
    amount: Decimal,
    parent_block_id: &str,
) -> anyhow::Result<String> {
    // 1. Create a Mint payload with a single output (refund to recipient)
    //    This mints new tokens to compensate for the burned NFT
    let outputs = vec![TxOutput {
        address: recipient.to_string(),
        amount: amount.to_string(),
    }];

    let payload = PayloadEnvelope::Plain(PlainPayload::Mint { outputs });
    let payload_json = serde_json::to_string(&payload)?;

    // 2. Get parent blocks (use the burn block as parent)
    let parents = vec![parent_block_id.to_string()];

    // 3. Compute block ID
    let nonce = 0;
    let block_id = pms_utils::compute_block_id(&parents, &Some(payload), nonce);

    // 4. Create WireBlock
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

    // 5. Sign the block with Coordinator wallet
    let msg_to_sign = pms_wallet::signing_wire::canonical_wireblock_message(&wire_block);
    let signature_hex = state.node_wallet.sign(&msg_to_sign)?;
    wire_block.signature_hex = signature_hex;

    // 6. Submit the block
    match state.srv.adapter_arc().persist_block(&wire_block).await {
        Ok(PutResult::Inserted) => {
            let _ = state.srv.enqueue_broadcast(wire_block.id.clone()).await;
            crate::metrics::BLOCKS_PERSISTED.inc();
            Ok(block_id)
        }
        Ok(PutResult::AlreadyExists) => {
            Ok(block_id) // Block already exists, that's fine
        }
        Ok(PutResult::Rejected(reason)) => {
            anyhow::bail!("Refund block rejected: {}", reason)
        }
        Err(e) => {
            anyhow::bail!("Failed to persist refund block: {}", e)
        }
    }
}

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
    // 1. Vérif basique
    if req.token_id.len() != 64 {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid token_id length (must be 64 hex chars)",
        )
            .into_response();
    }

    // 1b. Validation Cube: Si c'est un cube et que des Authority keys sont configurées,
    //     vérifier la signature des attributs AVANT de chiffrer et persister.
    if req.metadata.nft_type.as_deref() == Some("cube") {
        let authority_keys = &state.settings.fees.authority_public_keys;

        if !authority_keys.is_empty() {
            // Import inline pour éviter les problèmes de dépendance
            use crate::burn_refund::{
                CubeExtra, attributes_to_signed_message, verify_authority_signature,
            };

            // Extraire et valider le champ extra
            let extra_str = match &req.metadata.extra {
                Some(s) => s,
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        "Cube NFT missing 'extra' field with signature",
                    )
                        .into_response();
                }
            };

            let cube_extra: CubeExtra = match serde_json::from_str(extra_str) {
                Ok(ce) => ce,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        format!("Invalid Cube 'extra' JSON: {e}"),
                    )
                        .into_response();
                }
            };

            let signature = match &cube_extra.signature {
                Some(s) => s,
                None => {
                    return (
                        StatusCode::BAD_REQUEST,
                        "Cube NFT missing Authority signature in 'extra'",
                    )
                        .into_response();
                }
            };

            // Vérifier la signature contre TOUTES les clés Authority (match ANY)
            let message = attributes_to_signed_message(&cube_extra.attributes);
            let is_valid = authority_keys
                .iter()
                .any(|pk| verify_authority_signature(&message, signature, pk));

            if !is_valid {
                tracing::warn!(
                    "🚫 Cube {} rejected: signature doesn't match any of {} Authority keys",
                    req.token_id,
                    authority_keys.len()
                );
                return (
                    StatusCode::FORBIDDEN,
                    "Invalid Authority signature for Cube attributes",
                )
                    .into_response();
            }

            tracing::info!("✅ Cube {} Authority signature verified", req.token_id);
        } else {
            // Pas d'Authority configurée - warning en mode Dev
            tracing::warn!(
                "⚠️ Cube {} minted without Authority validation (no authority_public_keys configured)",
                req.token_id
            );
        }
    }

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

            crate::metrics::BLOCKS_PERSISTED.inc();

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

            let response = serde_json::json!({
                "status": "inserted",
                "block_id": block_id
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
// BURN NFT ENDPOINT
// ═══════════════════════════════════════════════════════════════════════════

/// Réponse pour POST /v1/nft/burn
///
/// Contient le statut du burn et optionnellement les informations de remboursement
/// si le NFT brûlé est un Cube authentique (signature Authority valide).
#[derive(Debug, Serialize, Deserialize)]
pub struct BurnNftResponse {
    /// Statut de l'opération ("burned", "rejected", etc.)
    pub status: String,
    /// ID du bloc créé dans le DAG
    pub block_id: String,
    /// Token ID du NFT brûlé (si single burn) ou "batch" (si batch burn)
    pub token_id: String,
    /// Liste des token IDs brûlés (pour batch burn)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_ids: Option<Vec<String>>,
    /// Remboursement (si cube authentique avec signature Authority valide)
    /// Calculé via la formule: (weight * size * density) / 10000
    pub refund: Option<RefundPreview>,
}

/// Preview du remboursement pour un cube brûlé
#[derive(Debug, Serialize, Deserialize)]
pub struct RefundPreview {
    /// Montant du remboursement en PMS
    pub amount: String,
    /// Adresse destinataire du remboursement
    pub recipient: String,
}

/// POST /v1/nft/burn
///
/// Endpoint helper pour burn un NFT. Le client doit fournir un `WireBlock`
/// pré-signé contenant un payload `NftAction::Burn` ou `NftAction::BatchBurn`.
///
/// ## Flow
/// 1. Parse le WireBlock depuis le body JSON
/// 2. Vérifie que le payload contient bien un `NftAction::Burn` ou `BatchBurn`
/// 3. Soumet le bloc au DAG via `persist_block`
/// 4. Si le cube est authentique (signature Authority), calcule le refund preview global
///
/// ## Pourquoi le client doit-il signer ?
/// La validation NFT (voir `pms-core/src/validations/nft.rs`)
/// exige que le `signer` du bloc soit égal au `burner`. Le serveur ne peut
/// donc pas signer à la place du client.
pub async fn burn_nft(
    State(state): State<AppState>,
    Json(wb): Json<WireBlock>,
) -> impl IntoResponse {
    // ─────────────────────────────────────────────────────────────────────
    // 1. EXTRACTION : Parser le payload pour vérifier que c'est un Burn
    // ─────────────────────────────────────────────────────────────────────
    // On récupère le payload JSON du bloc et on le désérialise
    // pour s'assurer qu'il contient bien une action NftAction::Burn.
    use pms_storage::NftStorage;
    use rust_decimal::Decimal;

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

    // Désérialiser le PayloadEnvelope
    // Note: On utilise `serde_json::from_str` car payload_json est une String
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

    // Extraire l'action NFT du payload (doit être Plain::Nft pour le burn)
    // PlainPayload est un enum défini dans pms-types-payload/src/payload.rs
    // On matche sur le variant Nft qui contient directement une NftAction
    let nft_action = match envelope {
        PayloadEnvelope::Plain(pms_types_payload::PlainPayload::Nft(action)) => action,
        PayloadEnvelope::Plain(_) => {
            // Le payload est Plain mais pas une action NFT
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Payload is not an NftAction (expected PlainPayload::Nft)"
                })),
            )
                .into_response();
        }
        PayloadEnvelope::Encrypted(_) => {
            // Le burn ne devrait pas être chiffré (pas de métadonnées à protéger)
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Burn action should not be encrypted"
                })),
            )
                .into_response();
        }
    };

    // Vérifier que c'est bien un Burn ou BatchBurn
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

    // ─────────────────────────────────────────────────────────────────────
    // 1.5. ANTI DOUBLE-REFUND : Vérifier que TOUS les NFTs existent encore
    // ─────────────────────────────────────────────────────────────────────
    // Si un seul NFT n'existe plus, on rejette tout le bloc.
    {
        for tid in &token_ids_to_process {
            match state.store.get_owner(tid) {
                Ok(Some(_)) => {
                    // exists
                }
                Ok(None) => {
                    // n'existe plus
                    tracing::warn!(
                        "🚫 Batch burn failure: Token {} already burned or missing",
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

    // ─────────────────────────────────────────────────────────────────────
    // 2. PRE-CALCUL DU REFUND (AVANT persist_block !)
    // ─────────────────────────────────────────────────────────────────────
    // On doit calculer le refund pour chaque token et sommer les montants.
    // Le coordinateur déchiffre les métadonnées depuis le bloc DAG.

    let mut total_refund: Decimal = Decimal::ZERO;
    let mut refund_recipient: Option<String> = None;

    for tid in &token_ids_to_process {
        // Déchiffrer les métadonnées depuis le bloc DAG
        // Le coordinateur peut déchiffrer car il est dans la liste des recipients
        let metadata = decrypt_nft_metadata_from_dag(&state, tid).await;

        let result = crate::burn_refund::calculate_burn_refund(
            tid,
            &burner,
            metadata.as_ref(),
            &state.settings.fees.authority_public_keys,
        );

        match result {
            Ok(Some(r)) => {
                total_refund += r.amount;
                // Le recipient doit être le burner (vérifié par calculate_burn_refund)
                if refund_recipient.is_none() {
                    refund_recipient = Some(r.recipient);
                }
                tracing::info!(
                    "🔥 Cube refund calculated: {} -> {} PMS",
                    &tid[..16.min(tid.len())],
                    r.amount
                );
            }
            Ok(None) => {
                // Pas de refund pour ce token (pas un cube ou pas authentique)
                tracing::debug!("No refund for token {}", tid);
            }
            Err(e) => {
                tracing::warn!("Refund calculation error for {}: {}", tid, e);
                // On continue mais sans refund pour ce token
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // 3. SOUMISSION : Persister le bloc dans le DAG
    // ─────────────────────────────────────────────────────────────────────
    // On délègue la validation globale au DAG.

    let block_id = wb.id.clone();

    match state.srv.adapter_arc().persist_block(&wb).await {
        Ok(pms_storage::PutResult::Inserted) => {
            // Broadcast
            let _ = state.srv.enqueue_broadcast(block_id.clone()).await;
            crate::metrics::BLOCKS_PERSISTED.inc();

            // ─────────────────────────────────────────────────────────────
            // 4. UPDATE STORE : Marquer comme brûlés
            // ─────────────────────────────────────────────────────────────
            if let Err(e) = state.store.apply_action(&nft_action) {
                tracing::error!("❌ Failed to apply BURN action to NFT store: {}", e);
            } else {
                tracing::info!(
                    "✅ {} NFTs marked as burned by {}",
                    token_ids_to_process.len(),
                    burner
                );
            }

            // ─────────────────────────────────────────────────────────────
            // 5. REFUND ALLOCATION - Create immediate UTXO for refund
            // ─────────────────────────────────────────────────────────────
            let refund_preview = if !total_refund.is_zero() && refund_recipient.is_some() {
                let recipient = refund_recipient.clone().unwrap();

                // Create a refund transaction block immediately
                // This creates a UTXO for the burner with the refund amount
                match create_refund_utxo_block(
                    &state,
                    &recipient,
                    total_refund,
                    &block_id,
                ).await {
                    Ok(refund_block_id) => {
                        tracing::info!(
                            "🔥 Refund UTXO created: {} PMS -> {} (block: {})",
                            total_refund,
                            &recipient[..20.min(recipient.len())],
                            &refund_block_id[..16]
                        );
                    }
                    Err(e) => {
                        tracing::error!("❌ Failed to create refund UTXO: {}", e);
                        // Fallback: add to fee pool for later distribution
                        let mut pool = state.fee_pool.write().await;
                        pool.add_burn_refund(&recipient, total_refund);
                    }
                }

                Some(RefundPreview {
                    amount: total_refund.to_string(),
                    recipient,
                })
            } else {
                None
            };

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
                refund: refund_preview,
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
