use crate::api::AppState;
use crate::api_fn::tx_helpers;
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use http::{HeaderMap, StatusCode};
use pms_types::{OutputId, TxInput, TxOutput};
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;

fn is_admin_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    if let Some(ref token) = state.admin_token {
        if let Some(auth) = headers.get("authorization") {
            if let Ok(val) = auth.to_str() {
                return val.strip_prefix("Bearer ").is_some_and(|t| {
                    use subtle::ConstantTimeEq;
                    t.as_bytes().ct_eq(token.as_bytes()).into()
                });
            }
        }
        false
    } else {
        true // no token configured = allow all
    }
}

// ── Request structs ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct FreezeRequest {
    pub address: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct UnfreezeRequest {
    pub address: String,
    pub reason: String,
    pub freeze_block_id: String,
}

#[derive(Debug, Deserialize)]
pub struct SeizeRequest {
    pub address: String,
    pub reason: String,
    #[serde(default)]
    pub utxo_ids: Vec<UtxoRef>,
}

#[derive(Debug, Deserialize)]
pub struct UtxoRef {
    pub txid: String,
    pub index: u32,
}

#[derive(Debug, Deserialize)]
pub struct ReverseRequest {
    pub block_id: String,
    pub reason: String,
}

// ── POST /admin/compliance/freeze ────────────────────────────────────────

pub async fn admin_freeze(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<FreezeRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }

    if req.address.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "address is required"})),
        );
    }

    if state.store.is_frozen(&req.address).unwrap_or(false) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "address already frozen"})),
        );
    }

    let payload = PayloadEnvelope::Plain(PlainPayload::Freeze {
        address: req.address.clone(),
        reason: req.reason.clone(),
    });

    let parents = match tx_helpers::get_block_parents(&state.store, &state.settings).await {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    };

    let adapter = state.srv.adapter_arc();
    let wb = match tx_helpers::forge_and_sign_block(
        Some(payload),
        parents,
        &adapter,
        &state.node_wallet,
        &state.settings,
        Some("Compliance: Freeze"),
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    };

    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(pms_storage::PutResult::Inserted) => (
            StatusCode::OK,
            Json(json!({
                "status": "frozen",
                "block_id": wb.id,
                "address": req.address,
            })),
        ),
        Ok(pms_storage::PutResult::Rejected(reason)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": reason})),
        ),
        Ok(_) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "block already exists"})),
        ),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    }
}

// ── POST /admin/compliance/unfreeze ──────────────────────────────────────

pub async fn admin_unfreeze(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UnfreezeRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }

    if !state.store.is_frozen(&req.address).unwrap_or(false) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "address is not frozen"})),
        );
    }

    let payload = PayloadEnvelope::Plain(PlainPayload::Unfreeze {
        address: req.address.clone(),
        reason: req.reason.clone(),
        freeze_block_id: req.freeze_block_id.clone(),
    });

    let parents = match tx_helpers::get_block_parents(&state.store, &state.settings).await {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    };

    let adapter = state.srv.adapter_arc();
    let wb = match tx_helpers::forge_and_sign_block(
        Some(payload),
        parents,
        &adapter,
        &state.node_wallet,
        &state.settings,
        Some("Compliance: Unfreeze"),
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    };

    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(pms_storage::PutResult::Inserted) => (
            StatusCode::OK,
            Json(json!({
                "status": "unfrozen",
                "block_id": wb.id,
                "address": req.address,
            })),
        ),
        Ok(pms_storage::PutResult::Rejected(reason)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": reason})),
        ),
        Ok(_) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "block already exists"})),
        ),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    }
}

// ── POST /admin/compliance/seize ─────────────────────────────────────────

pub async fn admin_seize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SeizeRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }

    let adapter = state.srv.adapter_arc();

    // Resolve treasury address
    let treasury_addr = if !state.treasury_wallets.is_empty() {
        state.treasury_wallets.list[0].clone()
    } else if !state.settings.fees.treasury_addresses.is_empty() {
        state.settings.fees.treasury_addresses[0].clone()
    } else if !state.settings.admin.wallet_addresses.is_empty() {
        state.settings.admin.wallet_addresses[0].clone()
    } else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "no treasury address configured"})),
        );
    };

    // Fetch UTXOs to seize
    let all_utxos = adapter.utxos_by_address(&req.address).await;
    if all_utxos.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no UTXOs found for address"})),
        );
    }

    // Filter by specific utxo_ids if provided, otherwise seize all
    let target_utxos: Vec<(OutputId, TxOutput)> = if req.utxo_ids.is_empty() {
        all_utxos
    } else {
        let refs: std::collections::HashSet<(String, u32)> = req
            .utxo_ids
            .iter()
            .map(|r| (r.txid.clone(), r.index))
            .collect();
        all_utxos
            .into_iter()
            .filter(|(oid, _)| refs.contains(&(oid.txid.clone(), oid.index)))
            .collect()
    };

    if target_utxos.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "specified UTXOs not found"})),
        );
    }

    // Build inputs and outputs grouped by asset_id
    let inputs: Vec<TxInput> = target_utxos
        .iter()
        .map(|(oid, _)| TxInput {
            out: OutputId {
                txid: oid.txid.clone(),
                index: oid.index,
            },
        })
        .collect();

    let mut amounts_by_asset: std::collections::HashMap<Option<String>, Decimal> =
        std::collections::HashMap::new();
    for (_, out) in &target_utxos {
        if let Ok(amt) = Decimal::from_str_exact(&out.amount) {
            *amounts_by_asset
                .entry(out.asset_id.clone())
                .or_insert(Decimal::ZERO) += amt;
        }
    }

    let outputs: Vec<TxOutput> = amounts_by_asset
        .into_iter()
        .map(|(asset_id, amount)| TxOutput {
            address: treasury_addr.clone(),
            amount: amount.to_string(),
            asset_id,
        })
        .collect();

    let seized_count = target_utxos.len();
    let payload = PayloadEnvelope::Plain(PlainPayload::Seize {
        from_address: req.address.clone(),
        inputs,
        outputs: outputs.clone(),
        reason: req.reason.clone(),
    });

    let parents = match tx_helpers::get_block_parents(&state.store, &state.settings).await {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    };

    let wb = match tx_helpers::forge_and_sign_block(
        Some(payload),
        parents,
        &adapter,
        &state.node_wallet,
        &state.settings,
        Some("Compliance: Seize"),
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    };

    // Note: persist_block already handles UTXOs via UtxoDelta → apply_diff()
    // for plain Seize payloads. No manual apply_utxo_delta needed.
    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(pms_storage::PutResult::Inserted) => {
            (
                StatusCode::OK,
                Json(json!({
                    "status": "seized",
                    "block_id": wb.id,
                    "from_address": req.address,
                    "treasury_address": treasury_addr,
                    "seized_utxos_count": seized_count,
                })),
            )
        }
        Ok(pms_storage::PutResult::Rejected(reason)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": reason})),
        ),
        Ok(_) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "block already exists"})),
        ),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    }
}

// ── POST /admin/compliance/reverse ───────────────────────────────────────

pub async fn admin_reverse(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ReverseRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }

    let adapter = state.srv.adapter_arc();

    // 1. Load the original block
    let original_wb = match adapter.get_block(&req.block_id).await {
        Ok(Some(wb)) => wb,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "original block not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("store error: {e}")})),
            );
        }
    };

    // 2. Parse the original payload -- must be TxUtxo
    let original_tx = match original_wb
        .payload_json
        .as_ref()
        .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
    {
        Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx))) => tx,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "only TxUtxo transactions can be reversed"})),
            );
        }
    };

    // 3. Check that all outputs are still unspent
    for (idx, _out) in original_tx.outputs.iter().enumerate() {
        let oid = OutputId {
            txid: req.block_id.clone(),
            index: idx as u32,
        };
        if adapter.get_utxo(&oid).await.is_none() {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": format!("output {}#{} already spent, cannot reverse", req.block_id, idx),
                })),
            );
        }
    }

    // 4. Build the reverse: consume original outputs, recreate original inputs
    // inputs = the original tx's outputs (we're spending them back)
    let reverse_inputs: Vec<TxInput> = original_tx
        .outputs
        .iter()
        .enumerate()
        .map(|(idx, _)| TxInput {
            out: OutputId {
                txid: req.block_id.clone(),
                index: idx as u32,
            },
        })
        .collect();

    // outputs = recreate UTXOs for the original senders
    // We need to find the original input addresses by looking up the spent UTXOs
    let mut refund_amounts: std::collections::HashMap<(String, Option<String>), Decimal> =
        std::collections::HashMap::new();
    for inp in &original_tx.inputs {
        // The original inputs were spent, so we can't look them up in the UTXO set.
        // We need to reconstruct from the original tx outputs amounts.
        // For simplicity: look up the original UTXO value from the store's utxo_spent CF.
        // If not available, fall back to the tx metadata.
        // The UTXO was at inp.out, and should still be in utxo_spent with the block that spent it.
        // Actually, the address info might not be easily available. Let's use a simpler approach:
        // the total of original outputs minus fee = total of original inputs.
        // We refund to a single address from the original inputs if we can resolve it.
    }

    // Alternative approach: compute the reverse outputs directly from the original tx inputs.
    // Since the original inputs are already spent, we look them up in the store.
    let mut reverse_outputs: Vec<TxOutput> = Vec::new();
    for inp in &original_tx.inputs {
        // Try to find the original UTXO value from the store
        // The utxo_spent CF stores block_id as value, not the UTXO data.
        // We need to look at the block that created this UTXO.
        let creator_block = match adapter.get_block(&inp.out.txid).await {
            Ok(Some(wb)) => wb,
            _ => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": format!("cannot find creator block for input {}#{}", inp.out.txid, inp.out.index),
                    })),
                );
            }
        };

        // Parse the creator block to find the output at the given index
        let creator_output = match creator_block
            .payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
        {
            Some(PayloadEnvelope::Plain(PlainPayload::Mint { outputs })) => {
                outputs.get(inp.out.index as usize).cloned()
            }
            Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx))) => {
                tx.outputs.get(inp.out.index as usize).cloned()
            }
            Some(PayloadEnvelope::Plain(PlainPayload::Reward {
                fee_outputs,
                reward_outputs,
                ..
            })) => {
                let all: Vec<_> = fee_outputs
                    .iter()
                    .chain(reward_outputs.iter())
                    .cloned()
                    .collect();
                all.get(inp.out.index as usize).cloned()
            }
            Some(PayloadEnvelope::Plain(PlainPayload::BridgeMint { outputs, .. })) => {
                outputs.get(inp.out.index as usize).cloned()
            }
            Some(PayloadEnvelope::Plain(PlainPayload::Seize { outputs, .. })) => {
                outputs.get(inp.out.index as usize).cloned()
            }
            Some(PayloadEnvelope::Plain(PlainPayload::Reverse { outputs, .. })) => {
                outputs.get(inp.out.index as usize).cloned()
            }
            _ => None,
        };

        match creator_output {
            Some(out) => {
                if let Ok(amt) = Decimal::from_str_exact(&out.amount) {
                    let key = (out.address.clone(), out.asset_id.clone());
                    *refund_amounts.entry(key).or_insert(Decimal::ZERO) += amt;
                }
            }
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": format!("cannot resolve original output {}#{}", inp.out.txid, inp.out.index),
                    })),
                );
            }
        }
    }

    for ((address, asset_id), amount) in &refund_amounts {
        reverse_outputs.push(TxOutput {
            address: address.clone(),
            amount: amount.to_string(),
            asset_id: asset_id.clone(),
        });
    }

    let payload = PayloadEnvelope::Plain(PlainPayload::Reverse {
        original_block_id: req.block_id.clone(),
        inputs: reverse_inputs.clone(),
        outputs: reverse_outputs.clone(),
        reason: req.reason.clone(),
    });

    let parents = match tx_helpers::get_block_parents(&state.store, &state.settings).await {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    };

    let wb = match tx_helpers::forge_and_sign_block(
        Some(payload),
        parents,
        &adapter,
        &state.node_wallet,
        &state.settings,
        Some("Compliance: Reverse"),
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    };

    // Note: persist_block already handles UTXOs via UtxoDelta → apply_diff()
    // for plain Reverse payloads. No manual apply_utxo_delta needed.
    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(pms_storage::PutResult::Inserted) => {
            let refunded: Vec<String> = refund_amounts.keys().map(|(a, _)| a.clone()).collect();
            (
                StatusCode::OK,
                Json(json!({
                    "status": "reversed",
                    "block_id": wb.id,
                    "reversed_block_id": req.block_id,
                    "refunded_addresses": refunded,
                })),
            )
        }
        Ok(pms_storage::PutResult::Rejected(reason)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": reason})),
        ),
        Ok(_) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "block already exists"})),
        ),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))),
    }
}

// ── GET /admin/compliance/frozen ─────────────────────────────────────────

pub async fn admin_list_frozen(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }

    match state.store.list_frozen() {
        Ok(entries) => (
            StatusCode::OK,
            Json(json!({
                "frozen_accounts": entries.len(),
                "accounts": entries,
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

// ── GET /admin/compliance/log ────────────────────────────────────────────

pub async fn admin_compliance_log(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }

    match state.store.list_compliance_log() {
        Ok(entries) => (
            StatusCode::OK,
            Json(json!({
                "total_entries": entries.len(),
                "log": entries,
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

// ── GET /admin/compliance/shadow_balance ──────────────────────────────────

pub async fn admin_shadow_balance(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }

    let frozen = match state.store.list_frozen() {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            );
        }
    };

    let adapter = state.srv.adapter_arc();
    let mut accounts = Vec::new();
    let mut total_pms = Decimal::ZERO;

    for f in &frozen {
        let utxos = adapter.utxos_by_address(&f.address).await;
        let mut by_asset: std::collections::HashMap<Option<String>, (Decimal, u64)> =
            std::collections::HashMap::new();

        for (_, out) in &utxos {
            if let Ok(amt) = Decimal::from_str_exact(&out.amount) {
                let entry = by_asset
                    .entry(out.asset_id.clone())
                    .or_insert((Decimal::ZERO, 0));
                entry.0 += amt;
                entry.1 += 1;
            }
        }

        let pms_bal = by_asset.get(&None).map(|v| v.0).unwrap_or(Decimal::ZERO);
        total_pms += pms_bal;

        let assets: Vec<serde_json::Value> = by_asset
            .into_iter()
            .map(|(asset_id, (bal, count))| {
                json!({
                    "asset_id": asset_id,
                    "balance": bal.to_string(),
                    "utxo_count": count,
                })
            })
            .collect();

        accounts.push(json!({
            "address": f.address,
            "pms_balance": pms_bal.to_string(),
            "frozen_since_ms": f.frozen_at_ms,
            "reason": f.reason,
            "freeze_block_id": f.block_id,
            "assets": assets,
        }));
    }

    (
        StatusCode::OK,
        Json(json!({
            "frozen_accounts": accounts.len(),
            "total_pms_frozen": total_pms.to_string(),
            "accounts": accounts,
        })),
    )
}
