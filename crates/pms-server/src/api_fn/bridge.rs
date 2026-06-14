use crate::api::AppState;
use crate::helper::is_admin_authorized;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use pms_bridge::engine::BridgeEngine;
use pms_bridge::store::BridgeStore;
use pms_bridge::types::{BridgeDisableRequest, BridgeEnableRequest, BridgeTransferRequest};
use pms_storage::ConfigStorage;
use rust_decimal::Decimal;
use serde_json::json;

/// Validates a raw `cross_ledger_fee_multiplier` config value and converts
/// it to a `Decimal` suitable for fee arithmetic.
///
/// Returns:
/// * `Ok(Some(dec))` — multiplier is a finite positive value that fits in `Decimal`.
/// * `Ok(None)` — multiplier is exactly `0.0`, the operator-intended way to
///   disable the cross-ledger surcharge entirely.
/// * `Err(reason)` — multiplier is NaN, ±Inf, negative, or too large to
///   represent as `Decimal`. The caller must surface this loudly and
///   skip the fee rather than silently default to something like `2×`.
fn validate_cross_ledger_multiplier(multiplier: f64) -> Result<Option<Decimal>, &'static str> {
    if multiplier.is_nan() {
        return Err("value is NaN");
    }
    if !multiplier.is_finite() {
        return Err("value is infinite");
    }
    if multiplier < 0.0 {
        return Err("value is negative");
    }
    if multiplier == 0.0 {
        return Ok(None);
    }
    match Decimal::from_f64_retain(multiplier) {
        Some(dec) => Ok(Some(dec)),
        None => Err("value cannot be represented as Decimal"),
    }
}

/// Helper: construit un BridgeEngine depuis l'AppState.
fn bridge_engine(state: &AppState) -> Option<BridgeEngine> {
    let mgr = state.ledger_mgr.as_ref()?;
    let default = mgr.default_ledger()?;
    let bridge_store = BridgeStore::new(default.store.clone());
    Some(BridgeEngine::new(
        mgr.clone(),
        bridge_store,
        state.node_wallet.clone(),
    ))
}

/// POST /admin/bridge/enable — Active un pont (admin, n'importe quels ledgers)
pub async fn admin_bridge_enable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<BridgeEnableRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let Some(engine) = bridge_engine(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "multi-ledger not enabled"})),
        )
            .into_response();
    };

    match engine.enable_bridge(&req, true, None) {
        Ok(link) => (StatusCode::CREATED, Json(json!(link))).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /admin/bridge/disable — Coupe un pont (admin)
pub async fn admin_bridge_disable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<BridgeDisableRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let Some(engine) = bridge_engine(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "multi-ledger not enabled"})),
        )
            .into_response();
    };

    match engine.disable_bridge(&req, true, None) {
        Ok(link) => (StatusCode::OK, Json(json!(link))).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /admin/bridge/transfer — Transfert cross-ledger (admin)
pub async fn admin_bridge_transfer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<BridgeTransferRequest>,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let Some(engine) = bridge_engine(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "multi-ledger not enabled"})),
        )
            .into_response();
    };

    // Kill-switch d'émission (gouvernance) — defense-in-depth. Un transfert
    // cross-ledger de PMS NATIF (`asset_id = None`) forge un `BridgeMint` non
    // gaté par `EmissionGate` ; tant que la réconciliation lock↔mint du bridge
    // n'est pas durcie, un `mint_enabled = false` doit aussi le bloquer (sinon
    // l'opérateur croit l'émission stoppée alors que le bridge peut encore créer
    // du PMS natif). Les transferts d'assets custom (token_id défini) ne sont pas
    // de l'émission de PMS natif → non gatés.
    if req.asset_id.is_none() {
        if let Ok(cfg) = state.store.get_runtime_config() {
            if !cfg.mint_enabled {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": "native mint is disabled by governance"})),
                )
                    .into_response();
            }
        }
    }

    match engine.execute_transfer(&req).await {
        Ok(resp) => {
            // Charge cross-ledger fee (base_fee * cross_ledger_multiplier).
            // Any pathological multiplier (NaN, ±Inf, negative, non-representable
            // as `Decimal`) is rejected with a loud log instead of falling
            // back to a silent 2× default — an invalid config must never
            // result in mystery fees being charged to users.
            let multiplier_f64 = state.settings.fees.cross_ledger_fee_multiplier;
            match validate_cross_ledger_multiplier(multiplier_f64) {
                Ok(Some(multiplier_dec)) => {
                    let (fee_policy, _ratio) =
                        crate::api_fn::tx_helpers::load_fee_policy(&state.store);
                    let base_fee = fee_policy
                        .compute_fee(&req.amount)
                        .map(|a| a.inner())
                        .unwrap_or(Decimal::ZERO);
                    let cross_fee = base_fee
                        .checked_mul(multiplier_dec)
                        .map(|m| m.round_dp(8))
                        .unwrap_or(Decimal::ZERO);

                    if cross_fee > Decimal::ZERO {
                        crate::api_fn::tx_helpers::accumulate_tx_fee(&state, cross_fee).await;
                        tracing::info!(
                            "Cross-ledger fee: {} PMS (x{} multiplier) accumulated for bridge {} -> {}",
                            cross_fee,
                            multiplier_f64,
                            req.from_ledger,
                            req.to_ledger
                        );
                    }
                }
                Ok(None) => {
                    // multiplier == 0.0 → operator explicitly disabled the surcharge.
                }
                Err(reason) => {
                    tracing::error!(
                        target = "pms_bridge",
                        multiplier = multiplier_f64,
                        from_ledger = %req.from_ledger,
                        to_ledger = %req.to_ledger,
                        %reason,
                        "Invalid cross_ledger_fee_multiplier — cross-ledger fee NOT charged. \
                         Fix [fees].cross_ledger_fee_multiplier in the config."
                    );
                }
            }
            (StatusCode::CREATED, Json(json!(resp))).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// GET /v1/bridge/links — Liste tous les ponts
pub async fn list_bridge_links(State(state): State<AppState>) -> impl IntoResponse {
    let Some(engine) = bridge_engine(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "multi-ledger not enabled"})),
        )
            .into_response();
    };

    match engine.list_bridges() {
        Ok(links) => (StatusCode::OK, Json(json!({"links": links}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// GET /v1/bridge/status/{lock_block_id} — Statut d'un transfert
pub async fn bridge_status(
    State(state): State<AppState>,
    axum::extract::Path(lock_block_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(engine) = bridge_engine(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "multi-ledger not enabled"})),
        )
            .into_response();
    };

    match engine.transfer_status(&lock_block_id) {
        Ok(Some(mint_id)) => (
            StatusCode::OK,
            Json(json!({
                "lock_block_id": lock_block_id,
                "mint_block_id": mint_id,
                "status": "completed"
            })),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "lock_block_id": lock_block_id,
                "status": "not_found"
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::validate_cross_ledger_multiplier;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn zero_means_disabled() {
        let r = validate_cross_ledger_multiplier(0.0);
        println!("multiplier=0.0 -> {:?}", r);
        assert!(matches!(r, Ok(None)));
    }

    #[test]
    fn valid_positive_finite_values_are_accepted() {
        for (input, expected) in [(2.0, "2"), (0.5, "0.5"), (1.0, "1"), (10.25, "10.25")] {
            let r = validate_cross_ledger_multiplier(input);
            println!("multiplier={} -> {:?}", input, r);
            let dec = r.expect("should be Ok").expect("should be Some");
            assert_eq!(dec, Decimal::from_str(expected).unwrap());
        }
    }

    #[test]
    fn nan_is_rejected() {
        let r = validate_cross_ledger_multiplier(f64::NAN);
        println!("multiplier=NaN -> {:?}", r);
        assert_eq!(r, Err("value is NaN"));
    }

    #[test]
    fn positive_infinity_is_rejected() {
        let r = validate_cross_ledger_multiplier(f64::INFINITY);
        println!("multiplier=+Inf -> {:?}", r);
        assert_eq!(r, Err("value is infinite"));
    }

    #[test]
    fn negative_infinity_is_rejected() {
        let r = validate_cross_ledger_multiplier(f64::NEG_INFINITY);
        println!("multiplier=-Inf -> {:?}", r);
        assert_eq!(r, Err("value is infinite"));
    }

    #[test]
    fn negative_finite_is_rejected() {
        let r = validate_cross_ledger_multiplier(-1.5);
        println!("multiplier=-1.5 -> {:?}", r);
        assert_eq!(r, Err("value is negative"));
    }

    #[test]
    fn negative_zero_is_treated_as_zero() {
        // -0.0 == 0.0 in IEEE-754; our contract says "0 = disabled",
        // not "0 = invalid config". -0.0 must not be rejected.
        let r = validate_cross_ledger_multiplier(-0.0);
        println!("multiplier=-0.0 -> {:?}", r);
        assert!(matches!(r, Ok(None)));
    }

    #[test]
    fn huge_value_that_overflows_decimal_is_rejected() {
        // f64::MAX is well beyond Decimal's ~7.9e28 range.
        let r = validate_cross_ledger_multiplier(f64::MAX);
        println!("multiplier=f64::MAX -> {:?}", r);
        assert_eq!(r, Err("value cannot be represented as Decimal"));
    }
}
