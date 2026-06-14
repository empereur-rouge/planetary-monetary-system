//! Endpoints semi-fongibles (SFT façon ERC-1155 — `pms-spec-semi-fungibles.md`).
//!
//! - `POST /admin/sft/classes` — enregistre une classe (`SftClassCreate`).
//! - `POST /admin/sft/mint` — mint une quantité d'une classe (réutilise `Mint` ;
//!   persist enforce classe + `mint_authority` + `max_supply` via la même
//!   validation que les tokens).
//! - `GET /v1/sft/classes` / `GET /v1/sft/classes/{asset_id}` /
//!   `GET /v1/sft/collections/{collection}` — **publics** (catalogue).
//!
//! Transfert & burn d'une classe SFT réutilisent les endpoints existants
//! (`/v1/wallet/send-simple`, `/v1/wallet/token/burn`) avec
//! `asset_id = "collection:class"` — aucun code spécifique.

use crate::api::AppState;
use crate::api_error::ApiError;
use crate::api_fn::tx_helpers;
use axum::Json;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use pms_storage::{PutResult, SftClassStorage};
use pms_types::{PayloadEnvelope, PlainPayload, SftClass, TxOutput};
use pms_wallet::SignerBackend;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;

/// Requête de création de classe. `mint_authority`/`creator` sont fixés au
/// coordinateur (l'opérateur admin) côté serveur — non fournis par le client.
#[derive(Debug, Deserialize)]
pub struct CreateSftClassRequest {
    pub collection_id: String,
    pub class_id: String,
    pub name: String,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub attributes: Option<String>,
    #[serde(default)]
    pub decimals: u8,
    #[serde(default)]
    pub max_supply: Option<String>,
    /// Demurrage opt-in (bps/jour, ≤ 10000). `None`/`0` = pas de décote.
    #[serde(default)]
    pub demurrage_bps_per_day: Option<u32>,
}

/// Requête de mint d'une classe.
#[derive(Debug, Deserialize)]
pub struct MintSftRequest {
    /// `asset_id` = `"collection:class"`.
    pub asset_id: String,
    pub to: String,
    pub amount: String,
}

/// Forge + persiste un bloc SFT (Coordinator-signé), renvoie son id.
/// Même chemin partagé que la gouvernance / l'on-ramp (un seul point de vérité).
async fn forge_sft_block(
    state: &AppState,
    payload: PlainPayload,
    label: &str,
) -> Result<String, ApiError> {
    let parents = tx_helpers::get_block_parents(&state.store, &state.settings)
        .await
        .map_err(|e| ApiError::Internal { reason: format!("parents: {e}") })?;
    let wb = tx_helpers::forge_and_sign_block(
        Some(PayloadEnvelope::Plain(payload)),
        parents,
        &state.srv.adapter_arc(),
        &state.node_wallet,
        &state.settings,
        Some(label),
    )
    .await
    .map_err(|e| ApiError::Internal { reason: format!("forge: {e}") })?;
    match tx_helpers::persist_and_broadcast(state, &wb).await {
        Ok(PutResult::Inserted) => Ok(wb.id),
        Ok(PutResult::AlreadyExists) => Err(ApiError::AlreadyExists { kind: "block", id: wb.id }),
        Ok(PutResult::Rejected(r)) => Err(ApiError::Conflict(format!("sft rejected: {r}"))),
        Err(e) => Err(ApiError::StorageError { reason: e }),
    }
}

/// `POST /admin/sft/classes` — enregistre une nouvelle classe SFT.
pub async fn admin_create_sft_class(
    State(state): State<AppState>,
    Json(req): Json<CreateSftClassRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let asset_id = format!("{}:{}", req.collection_id, req.class_id);
    // L'opérateur (coordinateur) est créateur + autorité de mint — c'est lui qui
    // forge les blocs Mint, donc `mint_authority` DOIT être sa pubkey pour que la
    // validation de mint contraint (signer == mint_authority) passe.
    let coordinator_pk = state.node_wallet.encoded_public_key();
    let class = SftClass {
        asset_id: asset_id.clone(),
        collection_id: req.collection_id.clone(),
        class_id: req.class_id.clone(),
        name: req.name,
        uri: req.uri,
        attributes: req.attributes,
        decimals: req.decimals,
        max_supply: req.max_supply,
        demurrage_bps_per_day: req.demurrage_bps_per_day,
        creator: coordinator_pk.clone(),
        mint_authority: coordinator_pk,
    };
    // La validation (format, asset_id==collection:class, unicité…) vit dans persist.
    let block_id = forge_sft_block(&state, PlainPayload::SftClassCreate(class), "SftClassCreate").await?;
    Ok(Json(json!({
        "status": "ok",
        "asset_id": asset_id,
        "collection_id": req.collection_id,
        "class_id": req.class_id,
        "block_id": block_id,
    })))
}

/// `POST /admin/sft/mint` — mint une quantité d'une classe vers une adresse.
pub async fn admin_mint_sft(
    State(state): State<AppState>,
    Json(req): Json<MintSftRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let amount = Decimal::from_str_exact(&req.amount).ok().filter(|d| *d > Decimal::ZERO);
    let Some(amount) = amount else {
        return Err(ApiError::InvalidField {
            field: "amount",
            reason: "must be a positive decimal".into(),
        });
    };
    // Pré-check d'existence (404 lisible). Le CAP + l'autorité restent enforcés au
    // PROTOCOLE (persist : `Mint` validé contre la classe SFT via to_token_metadata) —
    // le pré-check ci-dessous ne fait qu'offrir un message clair à l'opérateur et
    // éviter de forger un bloc voué au rejet.
    let class = state
        .store
        .get_sft_class(&req.asset_id)
        .map_err(|e| ApiError::StorageError { reason: e.to_string() })?
        .ok_or_else(|| ApiError::NotFound { kind: "sft class", id: req.asset_id.clone() })?;

    // Pré-check du cap (la supply d'une classe SFT est publique → pas de fuite).
    if let Some(max_supply) = class.max_supply.as_deref().and_then(|s| Decimal::from_str_exact(s).ok()) {
        let (current, _) = state.srv.adapter_arc().circulating_supply_by_asset(Some(&req.asset_id)).await;
        if current + amount > max_supply {
            return Err(ApiError::InvalidField {
                field: "amount",
                reason: format!("would exceed max_supply (current={current}, requested={amount}, max={max_supply})"),
            });
        }
    }

    let outputs = vec![TxOutput::new(req.to.clone(), amount.to_string(), Some(req.asset_id.clone()))];
    let block_id = forge_sft_block(&state, PlainPayload::Mint { outputs }, "SftMint").await?;
    Ok(Json(json!({
        "status": "ok",
        "asset_id": req.asset_id,
        "amount": amount.to_string(),
        "to": req.to,
        "block_id": block_id,
    })))
}

/// `GET /v1/sft/classes` — **public** : toutes les classes.
pub async fn list_sft_classes(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let classes = state
        .store
        .list_sft_classes()
        .map_err(|e| ApiError::StorageError { reason: e.to_string() })?;
    Ok(Json(json!({ "count": classes.len(), "classes": classes })))
}

/// `GET /v1/sft/classes/{asset_id}` — **public** : détail d'une classe.
pub async fn get_sft_class(
    State(state): State<AppState>,
    Path(asset_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    match state
        .store
        .get_sft_class(&asset_id)
        .map_err(|e| ApiError::StorageError { reason: e.to_string() })?
    {
        Some(class) => Ok(Json(json!(class))),
        None => Err(ApiError::NotFound { kind: "sft class", id: asset_id }),
    }
}

/// `GET /v1/sft/collections/{collection}` — **public** : classes d'une collection.
pub async fn list_sft_collection(
    State(state): State<AppState>,
    Path(collection): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let classes = state
        .store
        .list_sft_classes_by_collection(&collection)
        .map_err(|e| ApiError::StorageError { reason: e.to_string() })?;
    Ok(Json(json!({ "collection": collection, "count": classes.len(), "classes": classes })))
}
