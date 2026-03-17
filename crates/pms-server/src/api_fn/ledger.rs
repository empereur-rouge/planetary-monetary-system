use crate::api::AppState;
use crate::api_fn::tx_helpers;
use crate::helper::is_admin_authorized;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use pms_config::LedgerDef;
use pms_types_payload::{EncryptedPayload, OwnershipTransferData, PayloadEnvelope, PlainPayload};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize)]
pub struct LedgerInfo {
    pub id: String,
    pub network_id: String,
    pub prefix: String,
    pub protocol_version: u32,
    pub block_count: usize,
    pub symbol: String,
}

#[derive(Deserialize)]
pub struct LedgerQuery {
    pub search: Option<String>,
}

/// GET /v1/ledgers — Liste tous les ledgers actifs.
///
/// Query params:
/// - `search`: filtre prefix (starts_with, case-insensitive) sur id, network_id ou symbol
pub async fn list_ledgers(
    State(state): State<AppState>,
    Query(q): Query<LedgerQuery>,
) -> impl IntoResponse {
    let search = q.search.map(|s| s.to_lowercase());

    let Some(mgr) = &state.ledger_mgr else {
        let symbol = state
            ._cfg
            .network
            .symbol
            .clone()
            .unwrap_or_else(|| "PMS".into());
        let single = LedgerInfo {
            id: "main".into(),
            network_id: state._cfg.network.network_id.clone(),
            prefix: String::new(),
            protocol_version: state._cfg.network.protocol_version,
            block_count: 0,
            symbol,
        };
        let ledgers = if let Some(ref s) = search {
            if single.id.to_lowercase().starts_with(s)
                || single.network_id.to_lowercase().starts_with(s)
                || single.symbol.to_lowercase().starts_with(s)
            {
                vec![single]
            } else {
                vec![]
            }
        } else {
            vec![single]
        };
        return Json(json!({ "ledgers": ledgers }));
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
            symbol: l.def.symbol.clone().unwrap_or_else(|| "PMS".into()),
        })
        .filter(|l| {
            let Some(ref s) = search else { return true };
            l.id.to_lowercase().starts_with(s)
                || l.network_id.to_lowercase().starts_with(s)
                || l.symbol.to_lowercase().starts_with(s)
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
    /// Native token symbol for this ledger (default: "PMS")
    #[serde(default)]
    pub symbol: Option<String>,
    /// Clé publique (Ed25519) du propriétaire du ledger.
    /// `None` = admin-owned, `Some(pubkey)` = custom ledger avec owner.
    #[serde(default)]
    pub owner_pubkey: Option<String>,
    /// Clé publique X25519 du propriétaire (pour chiffrement des blocs d'ownership transfer).
    #[serde(default)]
    pub owner_x25519_pubkey: Option<String>,
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
        return (StatusCode::OK, Json(json!({"ledgers": [], "count": 0}))).into_response();
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
                "symbol": l.def.symbol.clone().unwrap_or_else(|| "PMS".into()),
                "tip_limit": l.def.tip_limit,
                "block_count": l.dag.len(),
                "utxo_shards": 256,
                "owner_pubkey": l.def.owner_pubkey,
            })
        })
        .collect();

    let count = ledgers.len();
    (
        StatusCode::OK,
        Json(json!({"ledgers": ledgers, "count": count})),
    )
        .into_response()
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
            "symbol": instance.def.symbol.clone().unwrap_or_else(|| "PMS".into()),
            "tip_limit": instance.def.tip_limit,
            "block_count": instance.dag.len(),
            "utxo_shards": 256,
            "owner_pubkey": instance.def.owner_pubkey,
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
        owner_pubkey: req.owner_pubkey.clone(),
        owner_x25519_pubkey: req.owner_x25519_pubkey.clone(),
        symbol: req.symbol.clone(),
    };

    match mgr.add_ledger(def.clone()).await {
        Ok(instance) => {
            // Auto-create gas pool (balance=0) for the new ledger
            {
                use pms_storage::GasPoolStorage;
                let now_ms = chrono::Utc::now().timestamp_millis();
                let pool = pms_types_economics::GasPool::new(req.id.clone(), now_ms);
                if let Err(e) = state.store.put_gas_pool(&pool) {
                    tracing::warn!("Failed to create gas pool for ledger '{}': {e}", req.id);
                }
            }

            // Persist ledger definition to RocksDB (survives restart)
            {
                use pms_storage::LedgerDefStorage;
                if let Err(e) = state.store.put_ledger_def(&def) {
                    tracing::warn!("Failed to persist ledger def for '{}': {e}", req.id);
                }
            }

            (
                StatusCode::CREATED,
                Json(json!({
                    "status": "ok",
                    "ledger": {
                        "id": instance.id,
                        "network_id": instance.def.network_id,
                        "prefix": instance.def.prefix,
                        "protocol_version": instance.def.protocol_version,
                        "block_count": instance.dag.len(),
                        "owner_pubkey": instance.def.owner_pubkey,
                    },
                    "message": "Ledger created. API routes available at /l/{id}/..., P2P routing active immediately."
                })),
            )
                .into_response()
        }
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

// ═══════════════════════════════════════════════════════════════════════════
// LEDGER OWNERSHIP TRANSFER (DAG-based, encrypted)
// ═══════════════════════════════════════════════════════════════════════════

/// Requête pour POST /admin/ledgers/:id/transfer-ownership.
#[derive(Deserialize)]
pub struct TransferOwnershipRequest {
    /// Nouvelle clé publique (Ed25519) du propriétaire. `None` = retour à admin-owned.
    pub new_owner_pubkey: Option<String>,
    /// Clé publique X25519 du nouveau propriétaire (pour chiffrement futur).
    #[serde(default)]
    pub new_owner_x25519_pubkey: Option<String>,
    /// Raison du transfert (audit trail, stocké chiffré dans le DAG).
    #[serde(default = "default_transfer_reason")]
    pub reason: String,
}

fn default_transfer_reason() -> String {
    "ownership transfer".to_string()
}

/// POST /admin/ledgers/{ledger_id}/transfer-ownership
///
/// Transfère la propriété d'un ledger custom à un nouveau propriétaire.
///
/// # Architecture DAG
/// Le transfert est enregistré comme un bloc `LedgerOwnershipTransfer` dans le DAG.
/// Le contenu sensible (new_owner_pubkey) est chiffré via X25519+AES-256-GCM.
/// Seuls le propriétaire actuel et le coordinateur peuvent déchiffrer le bloc.
///
/// # Flux
/// 1. Chiffrement des données de transfert pour owner + coordinator
/// 2. Création et signature d'un bloc DAG `LedgerOwnershipTransfer`
/// 3. Persistance du bloc dans le DAG (audit trail immuable)
/// 4. Application de l'état : RocksDB (ledger_defs) + mémoire (LedgerManager)
///
/// # Sécurité
/// - Admin-only (Bearer token via `require_local_or_admin` middleware).
/// - Le bloc est signé par le coordinateur.
/// - Le changement est tracé dans le DAG pour auditabilité.
pub async fn transfer_ledger_ownership(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(ledger_id): Path<String>,
    Json(req): Json<TransferOwnershipRequest>,
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

    let Some(instance) = mgr.get(&ledger_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("ledger '{}' not found", ledger_id)})),
        )
            .into_response();
    };

    let old_owner = instance.def.owner_pubkey.clone();
    let old_owner_x25519 = instance.def.owner_x25519_pubkey.clone();

    // ── 1. Build encrypted transfer data ──────────────────────────────
    let transfer_data = OwnershipTransferData {
        new_owner_pubkey: req.new_owner_pubkey.clone(),
        reason: req.reason.clone(),
    };
    let transfer_json = match serde_json::to_vec(&transfer_data) {
        Ok(j) => j,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("serialize transfer data: {e}")})),
            )
                .into_response();
        }
    };

    // Recipients: coordinator X25519 + current owner X25519 (if available)
    let coordinator_x25519 = state.node_wallet.x25519_pub_hex().to_string();
    let mut recipients = vec![coordinator_x25519];
    if let Some(ref owner_xpk) = old_owner_x25519 {
        if !owner_xpk.is_empty() && !recipients.contains(owner_xpk) {
            recipients.push(owner_xpk.clone());
        }
    }
    // Also add new owner X25519 if provided (so they can prove ownership)
    if let Some(ref new_xpk) = req.new_owner_x25519_pubkey {
        if !new_xpk.is_empty() && !recipients.contains(new_xpk) {
            recipients.push(new_xpk.clone());
        }
    }

    let encrypted_transfer = match EncryptedPayload::encrypt_for(
        &transfer_json,
        &recipients,
        transfer_json.len() as u32,
    ) {
        Ok(ep) => ep,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("encrypt transfer data: {e}")})),
            )
                .into_response();
        }
    };

    // ── 2. Create DAG block ───────────────────────────────────────────
    let payload = PayloadEnvelope::Plain(PlainPayload::LedgerOwnershipTransfer {
        ledger_id: ledger_id.clone(),
        encrypted_transfer,
    });

    let parents = match tx_helpers::get_block_parents(&state.store, &state.settings).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("get parents: {e}")})),
            )
                .into_response();
        }
    };

    let adapter = state.srv.adapter_arc();
    let wb = match tx_helpers::forge_and_sign_block(
        Some(payload),
        parents,
        &adapter,
        &state.node_wallet,
        &state.settings,
        Some(&format!(
            "LedgerOwnershipTransfer: {} → {:?}",
            ledger_id, req.new_owner_pubkey
        )),
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("forge block: {e}")})),
            )
                .into_response();
        }
    };

    // ── 3. Persist block to DAG (audit trail) ─────────────────────────
    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(pms_storage::PutResult::Inserted) => {}
        Ok(pms_storage::PutResult::Rejected(reason)) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error": format!("block rejected: {reason}")})),
            )
                .into_response();
        }
        Ok(_) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": "block already exists"})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("persist block: {e}")})),
            )
                .into_response();
        }
    }

    // ── 4. Apply state change (RocksDB + RAM) ─────────────────────────
    // The block is now persisted in the DAG. Apply the ownership change.
    let mut updated_def = instance.def.clone();
    updated_def.owner_pubkey = req.new_owner_pubkey.clone();
    updated_def.owner_x25519_pubkey = req.new_owner_x25519_pubkey.clone();

    {
        use pms_storage::LedgerDefStorage;
        if let Err(e) = state.store.put_ledger_def(&updated_def) {
            tracing::error!(
                "Failed to persist ownership change for '{}' (block {} already in DAG): {e}",
                ledger_id,
                wb.id
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": format!("state update failed (block {} persisted): {e}", wb.id),
                    "block_id": wb.id,
                })),
            )
                .into_response();
        }
    }

    mgr.update_def(&ledger_id, updated_def);

    tracing::info!(
        "🔑 Ledger '{}' ownership transferred: {:?} → {:?} (block {})",
        ledger_id,
        old_owner,
        req.new_owner_pubkey,
        wb.id,
    );

    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "block_id": wb.id,
            "ledger_id": ledger_id,
            "old_owner": old_owner,
            "new_owner": req.new_owner_pubkey,
        })),
    )
        .into_response()
}
