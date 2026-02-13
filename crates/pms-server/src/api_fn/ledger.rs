use crate::api::AppState;
use crate::helper::is_admin_authorized;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use pms_config::LedgerDef;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize)]
pub struct LedgerInfo {
    pub id: String,
    pub network_id: String,
    pub prefix: String,
    pub protocol_version: u32,
    pub block_count: usize,
}

/// GET /v1/ledgers — Liste tous les ledgers actifs.
pub async fn list_ledgers(State(state): State<AppState>) -> impl IntoResponse {
    let Some(mgr) = &state.ledger_mgr else {
        return Json(json!({
            "ledgers": [{
                "id": "main",
                "network_id": state._cfg.network.network_id,
                "prefix": "",
                "protocol_version": state._cfg.network.protocol_version,
                "block_count": 0,
            }]
        }));
    };

    let ledgers: Vec<LedgerInfo> = mgr
        .list_all()
        .iter()
        .map(|l| LedgerInfo {
            id: l.id.clone(),
            network_id: l.def.network_id.clone(),
            prefix: l.def.prefix.clone(),
            protocol_version: l.def.protocol_version,
            block_count: l.dag.len(),
        })
        .collect();

    Json(json!({ "ledgers": ledgers }))
}

// ═══════════════════════════════════════════════════════════════════════════
// ADMIN LEDGER API
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
pub struct CreateLedgerRequest {
    pub id: String,
    pub network_id: String,
    pub prefix: String,
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
    #[serde(default)]
    pub tip_limit: Option<usize>,
}

fn default_protocol_version() -> u32 {
    1
}

/// GET /admin/ledgers — Liste détaillée des ledgers (admin).
pub async fn admin_list_ledgers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let Some(mgr) = &state.ledger_mgr else {
        return (
            StatusCode::OK,
            Json(json!({"ledgers": [], "count": 0})),
        )
            .into_response();
    };

    let ledgers: Vec<serde_json::Value> = mgr
        .list_all()
        .iter()
        .map(|l| {
            json!({
                "id": l.id,
                "network_id": l.def.network_id,
                "prefix": l.def.prefix,
                "protocol_version": l.def.protocol_version,
                "tip_limit": l.def.tip_limit,
                "block_count": l.dag.len(),
                "utxo_shards": 256,
            })
        })
        .collect();

    let count = ledgers.len();
    (StatusCode::OK, Json(json!({"ledgers": ledgers, "count": count}))).into_response()
}

/// GET /admin/ledgers/{id} — Détail d'un ledger spécifique.
pub async fn admin_get_ledger(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(ledger_id): Path<String>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let Some(mgr) = &state.ledger_mgr else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "multi-ledger not enabled"})),
        )
            .into_response();
    };

    let Some(instance) = mgr.get(&ledger_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("ledger '{}' not found", ledger_id)})),
        )
            .into_response();
    };

    (
        StatusCode::OK,
        Json(json!({
            "id": instance.id,
            "network_id": instance.def.network_id,
            "prefix": instance.def.prefix,
            "protocol_version": instance.def.protocol_version,
            "tip_limit": instance.def.tip_limit,
            "block_count": instance.dag.len(),
            "utxo_shards": 256,
        })),
    )
        .into_response()
}

/// POST /admin/ledgers/create — Crée un nouveau ledger.
///
/// Le ledger est créé si ses column families existent déjà dans la DB partagée
/// (configuré au démarrage). Pour une création entièrement dynamique de CFs,
/// un redémarrage est nécessaire après ajout dans la config TOML.
pub async fn admin_create_ledger(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateLedgerRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let Some(mgr) = &state.ledger_mgr else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "multi-ledger not enabled"})),
        )
            .into_response();
    };

    // Validate
    if req.id.is_empty() || req.prefix.is_empty() || req.network_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "id, prefix, and network_id are required"})),
        )
            .into_response();
    }

    if mgr.get(&req.id).is_some() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": format!("ledger '{}' already exists", req.id)})),
        )
            .into_response();
    }

    let def = LedgerDef {
        id: req.id.clone(),
        network_id: req.network_id.clone(),
        prefix: req.prefix.clone(),
        protocol_version: req.protocol_version,
        tip_limit: req.tip_limit,
        fees: None,
        validation: None,
        owner_pubkey: None,
    };

    match mgr.add_ledger(def).await {
        Ok(instance) => (
            StatusCode::CREATED,
            Json(json!({
                "status": "ok",
                "ledger": {
                    "id": instance.id,
                    "network_id": instance.def.network_id,
                    "prefix": instance.def.prefix,
                    "protocol_version": instance.def.protocol_version,
                    "block_count": instance.dag.len(),
                },
                "message": "Ledger created. API routes available at /l/{id}/..., P2P routing active immediately."
            })),
        )
            .into_response(),
        Err(e) => {
            let msg = e.to_string();
            // If CFs don't exist, suggest adding to config and restarting
            if msg.contains("Column families") {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({
                        "error": msg,
                        "hint": "Add the ledger to [[ledgers]] in your config TOML and restart the server to create the required column families."
                    })),
                )
                    .into_response()
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": msg})),
                )
                    .into_response()
            }
        }
    }
}
