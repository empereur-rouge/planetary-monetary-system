//! Endpoints de gouvernance timelock (plan §4 / `pms-spec-governance-timelock.md`).
//!
//! - `POST /admin/governance/propose` — annonce un `GovernanceProposal` (bloc DAG
//!   signé Coordinator), timelocké selon le palier. N'applique RIEN.
//! - `POST /admin/governance/enact/{id}` — applique après expiration (rejeté avant).
//! - `POST /admin/governance/cancel/{id}` — annule une proposition `Pending`.
//! - `GET  /v1/governance/pending` — **public** : l'annonce (droit de sortie).
//! - `GET  /v1/governance/history` — **public** : enacted / cancelled (audit).
//!
//! Les durées de timelock sont celles du palier (`GovernanceTier::default_duration_ms`,
//! 7/15/45 j) — surcharge par config (testnet raccourci) en phase ultérieure.

use crate::api::AppState;
use crate::api_error::ApiError;
use crate::api_fn::tx_helpers;
use axum::Json;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use pms_config::{ConfigUpdate, GovernanceStatus, GovernanceTier};
use pms_storage::{GovernanceStorage, PutResult};
use pms_types::{PayloadEnvelope, PlainPayload};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

/// Requête de proposition : le changement, son palier, la justification.
#[derive(Debug, Deserialize)]
pub struct ProposeRequest {
    pub update: ConfigUpdate,
    pub tier: GovernanceTier,
    #[serde(default)]
    pub reason: String,
}

/// Corps commun enact / cancel.
#[derive(Debug, Deserialize)]
pub struct GovActionRequest {
    #[serde(default)]
    pub reason: String,
}

/// Forge + persiste un bloc de gouvernance (Coordinator-signé), puis renvoie son id.
async fn forge_governance_block(
    state: &AppState,
    payload: PlainPayload,
    label: &str,
) -> Result<String, ApiError> {
    let parents = tx_helpers::get_block_parents(&state.store, &state.settings)
        .await
        .map_err(|e| ApiError::Internal {
            reason: format!("parents: {e}"),
        })?;
    let wb = tx_helpers::forge_and_sign_block(
        Some(PayloadEnvelope::Plain(payload)),
        parents,
        &state.srv.adapter_arc(),
        &state.node_wallet,
        &state.settings,
        Some(label),
    )
    .await
    .map_err(|e| ApiError::Internal {
        reason: format!("forge: {e}"),
    })?;
    match tx_helpers::persist_and_broadcast(state, &wb).await {
        Ok(PutResult::Inserted) => Ok(wb.id),
        Ok(PutResult::AlreadyExists) => Err(ApiError::AlreadyExists {
            kind: "block",
            id: wb.id,
        }),
        // Le timelock + les checks de statut rejettent ici (enact trop tôt,
        // proposition non-pending, id inconnu…). La raison est non-sensible et
        // l'appelant est un opérateur authentifié → on la remonte verbatim pour
        // qu'il sache POURQUOI (timelock vs statut vs inconnu).
        Ok(PutResult::Rejected(r)) => Err(ApiError::GovernanceRejected { reason: r }),
        Err(e) => Err(ApiError::StorageError { reason: e }),
    }
}

/// `POST /admin/governance/propose` — annonce un changement timelocké.
pub async fn admin_propose(
    State(state): State<AppState>,
    Json(req): Json<ProposeRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // La cohérence du `ConfigUpdate` (ex: répartition des fees) est vérifiée à
    // l'enact, quand `apply_config_update` l'applique pour de vrai (validation
    // partagée) — inutile de la dupliquer ici.
    let announced_at_ms = pms_utils::ts_ms();
    let enact_after_ms = announced_at_ms.saturating_add(req.tier.default_duration_ms());

    // proposal_id = SHA-256(update + tier + announced_at) — déterministe.
    let id_input =
        serde_json::to_vec(&(&req.update, req.tier.as_str(), announced_at_ms)).unwrap_or_default();
    let proposal_id = hex::encode(Sha256::digest(&id_input));

    let block_id = forge_governance_block(
        &state,
        PlainPayload::GovernanceProposal {
            proposal_id: proposal_id.clone(),
            update: req.update,
            tier: req.tier,
            reason: req.reason.clone(),
            announced_at_ms,
            enact_after_ms,
        },
        "GovernanceProposal",
    )
    .await?;

    Ok(Json(json!({
        "status": "ok",
        "proposal_id": proposal_id,
        "block_id": block_id,
        "tier": req.tier.as_str(),
        "announced_at_ms": announced_at_ms,
        "enact_after_ms": enact_after_ms,
    })))
}

/// `POST /admin/governance/enact/{id}` — applique après expiration du timelock.
pub async fn admin_enact(
    State(state): State<AppState>,
    Path(proposal_id): Path<String>,
    Json(req): Json<GovActionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let block_id = forge_governance_block(
        &state,
        PlainPayload::GovernanceEnact {
            proposal_id: proposal_id.clone(),
            reason: req.reason,
        },
        "GovernanceEnact",
    )
    .await?;
    Ok(Json(json!({
        "status": "ok",
        "proposal_id": proposal_id,
        "block_id": block_id,
    })))
}

/// `POST /admin/governance/cancel/{id}` — annule une proposition `Pending`.
pub async fn admin_cancel(
    State(state): State<AppState>,
    Path(proposal_id): Path<String>,
    Json(req): Json<GovActionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let block_id = forge_governance_block(
        &state,
        PlainPayload::GovernanceCancel {
            proposal_id: proposal_id.clone(),
            reason: req.reason,
        },
        "GovernanceCancel",
    )
    .await?;
    Ok(Json(json!({
        "status": "ok",
        "proposal_id": proposal_id,
        "block_id": block_id,
    })))
}

/// Sérialise une proposition pour l'API publique.
fn proposal_json(r: &pms_config::GovernanceProposalRecord) -> serde_json::Value {
    json!({
        "proposal_id": r.proposal_id,
        "tier": r.tier.as_str(),
        "status": r.status.as_str(),
        "reason": r.reason,
        "announced_at_ms": r.announced_at_ms,
        "enact_after_ms": r.enact_after_ms,
        "update": r.update,
    })
}

/// Liste les propositions gardées par `keep`, sous la clé JSON `key`.
///
/// `list_governance_proposals` est un scan complet du CF qui désérialise chaque
/// record — acceptable ici : ces lectures sont à très faible QPS (gouvernance) et
/// la cardinalité est naturellement petite (quelques propositions par trimestre
/// vu les timelocks 7/15/45 j). À industrialiser via un index par statut si le
/// volume explose un jour.
fn list_filtered(
    state: &AppState,
    key: &'static str,
    keep: impl Fn(&pms_config::GovernanceProposalRecord) -> bool,
) -> Result<Json<serde_json::Value>, ApiError> {
    let all = state
        .store
        .list_governance_proposals()
        .map_err(|e| ApiError::StorageError {
            reason: e.to_string(),
        })?;
    let items: Vec<_> = all.iter().filter(|r| keep(r)).map(proposal_json).collect();
    Ok(Json(json!({ key: items })))
}

/// `GET /v1/governance/pending` — **public** : propositions en attente (l'annonce).
pub async fn list_pending(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    list_filtered(&state, "pending", |r| r.status == GovernanceStatus::Pending)
}

/// `GET /v1/governance/history` — **public** : propositions enacted / cancelled.
pub async fn list_history(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    list_filtered(&state, "history", |r| r.status != GovernanceStatus::Pending)
}
