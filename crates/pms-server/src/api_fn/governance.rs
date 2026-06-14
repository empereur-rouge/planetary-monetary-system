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
use pms_storage::{ConfigStorage, GovernanceStorage, PutResult};
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

/// Résultat d'une proposition forgée — partagé par l'endpoint `propose` et le
/// rewire de `admin_update_config`. `pub` pour les tests d'intégration.
pub struct ProposeOutcome {
    pub proposal_id: String,
    pub block_id: String,
    pub announced_at_ms: u64,
    pub enact_after_ms: u64,
    /// `true` si le timelock est nul (resserrage) — l'enact peut être immédiat.
    pub instant: bool,
}

/// Cœur du `propose` — réutilisé par l'endpoint HTTP ET par `admin_update_config`
/// (qui auto-assigne le palier). Valide le palier-min, dérive `enact_after` via
/// l'asymétrie, et forge le bloc `GovernanceProposal`. `pub` pour les tests.
pub async fn do_propose(
    state: &AppState,
    update: ConfigUpdate,
    tier: GovernanceTier,
    reason: String,
) -> Result<ProposeOutcome, ApiError> {
    // Palier minimum (table §2) — rejet immédiat si trop bas (évite de forger un
    // bloc voué au rejet par persist).
    if let Err(reason) = pms_config::validate_tier(&update, tier) {
        return Err(ApiError::GovernanceRejected { reason });
    }
    let announced_at_ms = pms_utils::ts_ms();
    // Asymétrie tighten/loosen : timelock instantané pour un resserrage, plein
    // sinon. MÊME fonction que la validation persist (un seul point de vérité).
    let current_cfg = state
        .store
        .get_runtime_config()
        .map_err(|e| ApiError::Internal { reason: format!("runtime config: {e}") })?;
    let timelock_ms = pms_config::required_timelock_ms(&update, &current_cfg, tier);
    let enact_after_ms = announced_at_ms.saturating_add(timelock_ms);

    // proposal_id = SHA-256(update + tier + announced_at) — déterministe.
    let id_input =
        serde_json::to_vec(&(&update, tier.as_str(), announced_at_ms)).unwrap_or_default();
    let proposal_id = hex::encode(Sha256::digest(&id_input));

    let block_id = forge_governance_block(
        state,
        PlainPayload::GovernanceProposal {
            proposal_id: proposal_id.clone(),
            update,
            tier,
            reason,
            announced_at_ms,
            enact_after_ms,
        },
        "GovernanceProposal",
    )
    .await?;

    Ok(ProposeOutcome {
        proposal_id,
        block_id,
        announced_at_ms,
        enact_after_ms,
        instant: timelock_ms == 0,
    })
}

/// Cœur de l'`enact` — réutilisé par l'endpoint HTTP, le rewire d'admin, et la
/// tâche d'auto-enact. Forge un bloc `GovernanceEnact` (la validation timelock +
/// l'application vivent dans `persist_block`). Renvoie l'id du bloc enact.
pub(crate) async fn do_enact(
    state: &AppState,
    proposal_id: &str,
    reason: String,
) -> Result<String, ApiError> {
    forge_governance_block(
        state,
        PlainPayload::GovernanceEnact {
            proposal_id: proposal_id.to_string(),
            reason,
        },
        "GovernanceEnact",
    )
    .await
}

/// `POST /admin/governance/propose` — annonce un changement timelocké.
pub async fn admin_propose(
    State(state): State<AppState>,
    Json(req): Json<ProposeRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let out = do_propose(&state, req.update, req.tier, req.reason).await?;
    Ok(Json(json!({
        "status": "ok",
        "proposal_id": out.proposal_id,
        "block_id": out.block_id,
        "tier": req.tier.as_str(),
        "announced_at_ms": out.announced_at_ms,
        "enact_after_ms": out.enact_after_ms,
    })))
}

/// `POST /admin/governance/enact/{id}` — applique après expiration du timelock.
pub async fn admin_enact(
    State(state): State<AppState>,
    Path(proposal_id): Path<String>,
    Json(req): Json<GovActionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let block_id = do_enact(&state, &proposal_id, req.reason).await?;
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

/// Sérialise une proposition pour l'API publique. Inclut les block ids du cycle
/// (proposal toujours ; enact/cancel selon le statut) pour permettre de remonter
/// au bloc DAG immuable correspondant.
fn proposal_json(r: &pms_config::GovernanceProposalRecord) -> serde_json::Value {
    json!({
        "proposal_id": r.proposal_id,
        "tier": r.tier.as_str(),
        "status": r.status.as_str(),
        "reason": r.reason,
        "announced_at_ms": r.announced_at_ms,
        "enact_after_ms": r.enact_after_ms,
        "update": r.update,
        "proposal_block_id": r.proposal_block_id,
        "enact_block_id": r.enact_block_id,
        "cancel_block_id": r.cancel_block_id,
    })
}

/// Construit les entrées « bloc de gouvernance » d'une proposition : le bloc
/// `proposal` (toujours), puis `enact`/`cancel` si présents. Chaque entrée est
/// dénormalisée (tier/status/update) pour faire un journal d'audit lisible en un
/// seul appel ; `block_id` permet de récupérer le bloc DAG brut via `/block/{id}`.
fn governance_block_entries(r: &pms_config::GovernanceProposalRecord) -> Vec<serde_json::Value> {
    let base = |kind: &str, block_id: &str| {
        json!({
            "block_id": block_id,
            "kind": kind,
            "proposal_id": r.proposal_id,
            "tier": r.tier.as_str(),
            "status": r.status.as_str(),
            "update": r.update,
            "reason": r.reason,
            "announced_at_ms": r.announced_at_ms,
            "enact_after_ms": r.enact_after_ms,
        })
    };
    let mut out = Vec::with_capacity(2);
    if !r.proposal_block_id.is_empty() {
        out.push(base("proposal", &r.proposal_block_id));
    }
    if let Some(id) = &r.enact_block_id {
        out.push(base("enact", id));
    }
    if let Some(id) = &r.cancel_block_id {
        out.push(base("cancel", id));
    }
    out
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

/// `GET /v1/governance/blocks` — **public** : TOUS les blocs DAG de gouvernance.
///
/// Renvoie le journal d'audit complet : pour chaque proposition, le bloc
/// `proposal` puis ses blocs `enact`/`cancel` éventuels, chacun avec son `block_id`
/// (récupérable via `/v1/block/{id}` pour le bloc brut signé). Les entrées sont
/// groupées **par proposition, dans l'ordre d'annonce** (`announced_at_ms`), le
/// cycle d'une proposition restant contigu (proposal→enact/cancel) — ce n'est donc
/// PAS un tri strictement chronologique par horodatage de bloc (un enact peut
/// suivre une annonce plus récente). Chaque entrée porte `announced_at_ms`,
/// `enact_after_ms` et le `block_id` immuable pour une reconstruction précise.
///
/// Construit depuis l'index `governance_proposals` (pas de scan du DAG) — voir
/// [`list_filtered`] pour les notes de coût (faible QPS, faible cardinalité).
pub async fn list_blocks(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let mut all = state
        .store
        .list_governance_proposals()
        .map_err(|e| ApiError::StorageError {
            reason: e.to_string(),
        })?;
    // Ordre chronologique stable par annonce (le cycle d'une proposition reste groupé).
    all.sort_by_key(|r| r.announced_at_ms);
    let blocks: Vec<_> = all.iter().flat_map(governance_block_entries).collect();
    Ok(Json(json!({ "count": blocks.len(), "blocks": blocks })))
}
