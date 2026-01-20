//! API endpoints pour les NFTs.
//!
//! Permet de query l'état des NFTs (ownership, existence).

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use pms_types_nft::NftMetadata;
use serde::{Deserialize, Serialize};

use crate::api::AppState;
use pms_interface::NetDagAdapter;
use pms_types_payload::{EncryptedPayload, PayloadEnvelope};
use pms_wallet::SignerBackend;
use pms_wire::WireBlock;

/// Réponse pour GET /v1/nft/{token_id}
#[derive(Debug, Serialize, Deserialize)]
pub struct NftResponse {
    /// Token ID demandé
    pub token_id: String,
    /// Propriétaire actuel (None si le token n'existe pas)
    pub owner: Option<String>,
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
            let response = NftResponse {
                token_id,
                owner: Some(owner),
                exists: true,
            };
            (StatusCode::OK, Json(response))
        }
        Ok(None) => {
            // Token n'existe pas
            let response = NftResponse {
                token_id,
                owner: None,
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
    let parents = match state.srv.adapter_arc().top_tips(2).await {
        Ok(tips) => tips,
        Err(_) => vec![],
    };

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
                use pms_types_nft::NftAction;

                // Construire l'action Mint avec les mêmes données qu'on a utilisées
                // pour le bloc (token_id, owner, metadata)
                let mint_action = NftAction::Mint {
                    token_id: req.token_id.clone(),
                    creator: req.owner_address.clone(),
                    metadata: req.metadata.clone(),
                };

                // Appliquer l'action au store NFT
                // Cela appelle set_owner + set_metadata (voir nft_store.rs lignes 82-91)
                if let Err(e) = state.store.apply_action(&mint_action) {
                    // Le bloc est déjà persisté, on log l'erreur mais on ne fail pas
                    // car le bloc est dans le DAG (source de vérité)
                    tracing::error!(
                        "❌ NFT store apply_action failed after block inserted: {}",
                        e
                    );
                } else {
                    tracing::info!(
                        "✅ NFT {} minted to {} (store updated)",
                        req.token_id,
                        req.owner_address
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
    /// Token ID du NFT brûlé
    pub token_id: String,
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
/// pré-signé contenant un payload `NftAction::Burn`.
///
/// ## Flow
/// 1. Parse le WireBlock depuis le body JSON
/// 2. Vérifie que le payload contient bien un `NftAction::Burn`
/// 3. Soumet le bloc au DAG via `persist_block`
/// 4. Si le cube est authentique (signature Authority), calcule le refund preview
///
/// ## Pourquoi le client doit-il signer ?
/// La validation NFT (voir `pms-core/src/validations/nft.rs` lignes 218-244)
/// exige que le `signer` du bloc soit égal au `burner`. Le serveur ne peut
/// donc pas signer à la place du client.
///
/// ## Voir aussi
/// - Chapitre 4.2 du Rust Book : Références et Emprunt
///   (pour comprendre pourquoi on passe `&wb` et `&state.store`)
pub async fn burn_nft(
    State(state): State<AppState>,
    Json(wb): Json<WireBlock>,
) -> impl IntoResponse {
    // ─────────────────────────────────────────────────────────────────────
    // 1. EXTRACTION : Parser le payload pour vérifier que c'est un Burn
    // ─────────────────────────────────────────────────────────────────────
    // On récupère le payload JSON du bloc et on le désérialise
    // pour s'assurer qu'il contient bien une action NftAction::Burn.

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

    // Vérifier que c'est bien un Burn
    let (token_id, burner) = match &nft_action {
        pms_types_nft::NftAction::Burn { token_id, burner } => (token_id.clone(), burner.clone()),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Expected NftAction::Burn, got different action type"
                })),
            )
                .into_response();
        }
    };

    // ─────────────────────────────────────────────────────────────────────
    // 1.5. ANTI DOUBLE-REFUND : Vérifier que le NFT existe encore
    // ─────────────────────────────────────────────────────────────────────
    // Si le NFT n'existe plus dans le store, c'est qu'il a déjà été brûlé.
    // On rejette immédiatement pour éviter un double-refund.
    //
    // Voir chapitre 6 du Rust Book : Enums and Pattern Matching
    // https://doc.rust-lang.org/book/ch06-00-enums.html
    {
        use pms_storage::NftStorage;

        match state.store.get_owner(&token_id) {
            Ok(Some(_)) => {
                // Le NFT existe, on peut continuer le processus de burn
            }
            Ok(None) => {
                // Le NFT n'existe plus → déjà brûlé !
                tracing::warn!(
                    "🚫 Double-burn attempt detected for token {}",
                    &token_id[..16.min(token_id.len())]
                );
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "NFT already burned or does not exist",
                        "token_id": token_id
                    })),
                )
                    .into_response();
            }
            Err(e) => {
                tracing::error!("Failed to check NFT existence: {}", e);
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

    // ─────────────────────────────────────────────────────────────────────
    // 2. PRE-CALCUL DU REFUND (AVANT persist_block !)
    // ─────────────────────────────────────────────────────────────────────
    // IMPORTANT: On doit calculer le refund AVANT de persister le bloc car
    // persist_block supprime les métadonnées du NFT (action Burn).
    // Si on calcule après, les métadonnées n'existent plus → "No metadata for token".

    let refund_result = crate::burn_refund::calculate_burn_refund(
        &token_id,
        &burner,
        state.store.as_ref(),
        &state.settings.fees.authority_public_keys,
    );

    // ─────────────────────────────────────────────────────────────────────
    // 3. SOUMISSION : Persister le bloc dans le DAG
    // ─────────────────────────────────────────────────────────────────────
    // On délègue la validation (signature, ownership) au DAG via persist_block.
    // Si le bloc est invalide (mauvaise signature, burner != owner, etc.),
    // persist_block retournera une erreur.

    let block_id = wb.id.clone();

    match state.srv.adapter_arc().persist_block(&wb).await {
        Ok(pms_storage::PutResult::Inserted) => {
            // Broadcast le bloc aux autres nœuds
            let _ = state.srv.enqueue_broadcast(block_id.clone()).await;
            crate::metrics::BLOCKS_PERSISTED.inc();

            // ─────────────────────────────────────────────────────────────
            // 4. REFUND PROCESSING : Ajouter au fee_pool si valide
            // ─────────────────────────────────────────────────────────────
            // On utilise le résultat pré-calculé (avant suppression des métadonnées).
            // On l'ajoute au fee_pool SEULEMENT si le persist a réussi.

            let refund_preview = match refund_result {
                Ok(Some(result)) => {
                    // Ajouter le refund au pool (sera distribué via Milestone)
                    // Utilise add_burn_refund (pas add_fee) car c'est pour un wallet utilisateur
                    {
                        let mut pool = state.fee_pool.write().await;
                        pool.add_burn_refund(&result.recipient, result.amount);
                        tracing::info!(
                            "🔥 Cube burn refund added to pool: {} -> {} PMS (token: {})",
                            &result.recipient[..20.min(result.recipient.len())],
                            result.amount,
                            &token_id[..16.min(token_id.len())]
                        );
                    }

                    Some(RefundPreview {
                        amount: result.amount.to_string(),
                        recipient: result.recipient,
                    })
                }
                Ok(None) => None, // Pas un cube ou signature invalide
                Err(e) => {
                    tracing::warn!("Refund calculation error for {}: {}", token_id, e);
                    None
                }
            };

            let response = BurnNftResponse {
                status: "burned".to_string(),
                block_id,
                token_id,
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
        Ok(pms_storage::PutResult::Rejected(reason)) => {
            // La validation a échoué (signature invalide, pas le owner, etc.)
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("Block rejected: {}", reason),
                    "block_id": block_id
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Storage error: {}", e)
            })),
        )
            .into_response(),
    }
}
