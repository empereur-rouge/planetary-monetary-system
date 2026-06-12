//! Preuve de réserves ancrée (protocole 2.6).
//!
//! Calcule périodiquement (ou à la demande) un snapshot agrégé de l'état
//! UTXO du ledger — `state_root` SHA-256 + supply totale par asset — et
//! l'ancre dans le DAG via un bloc [`PlainPayload::ReserveSnapshot`] signé
//! Coordinator. Un vérificateur indépendant recompute le root depuis le même
//! état et compare : tout mismatch prouve une divergence.
//!
//! ## Endpoints
//!
//! | Méthode | Path | Catégorie |
//! |---|---|---|
//! | `GET`  | `/v1/reserves/latest`     | public (lecture) |
//! | `POST` | `/admin/reserves/snapshot`| `admin_writable` (produit un bloc) |
//! | `POST` | `/admin/reserves/verify`  | `admin_recovery` (lecture seule) |
//!
//! ## Cohérence du calcul
//!
//! L'itération RocksDB du CF `utxo` se fait sur UN itérateur (vue
//! point-in-time consistante) : le `state_root` et les totaux par asset
//! proviennent du même instantané — pas de course avec les blocs en cours
//! d'écriture.

use crate::api::AppState;
use crate::helper::is_admin_authorized;
use crate::api_fn::tx_helpers;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use pms_types::{PayloadEnvelope, PlainPayload};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

/// Tag de domaine du hash de réserves (versionné).
const RESERVES_DOMAIN_TAG: &[u8] = b"pms-reserves-v1";

/// Résultat du calcul de réserves (état disque, point-in-time).
#[derive(Debug, Clone)]
pub struct ReserveComputation {
    /// SHA-256 (hex) de l'itération ordonnée clé+valeur du CF `utxo`.
    pub state_root: String,
    /// Supply totale par asset (`None` = natif du ledger).
    pub total_supply: Vec<(Option<String>, String)>,
    /// Nombre d'UTXOs couverts.
    pub utxo_count: u64,
}

/// Calcule `state_root` + totaux par asset depuis le CF `utxo` RocksDB
/// (autoritaire, jamais pruné). Tourne sur le pool blocking de tokio —
/// l'itération complète peut prendre plusieurs secondes sur un gros set.
pub async fn compute_reserves(
    store: Arc<pms_storage::rocks_store::store::RocksStore>,
) -> Result<ReserveComputation, String> {
    tokio::task::spawn_blocking(move || -> Result<ReserveComputation, String> {
        use std::sync::mpsc;
        const STREAM_BUFFER: usize = 10_000;

        let (tx, rx) = mpsc::sync_channel(STREAM_BUFFER);
        let store_for_stream = store.clone();
        let producer = std::thread::Builder::new()
            .name("reserves-stream".into())
            .spawn(move || store_for_stream.stream_all_utxos(tx))
            .map_err(|e| format!("spawn reserves stream thread: {e}"))?;

        let mut hasher = Sha256::new();
        hasher.update(RESERVES_DOMAIN_TAG);
        let mut totals: HashMap<Option<String>, Decimal> = HashMap::new();
        let mut count: u64 = 0;

        // L'ordre d'itération RocksDB (clés triées) rend le hash déterministe
        // pour un état donné, sans tri en RAM.
        while let Ok((txid, idx, uv)) = rx.recv() {
            hasher.update(txid.as_bytes());
            hasher.update(b"#");
            hasher.update(idx.to_be_bytes());
            hasher.update(b"|");
            // Ré-encode la valeur (au lieu de hasher les bytes bruts du CF) :
            // CANONICALISE volontairement — un UTXO legacy (sans champs
            // lkd/cond/cat) hashe identique à sa ré-écriture moderne. Coût :
            // un decode+encode JSON par UTXO, périodique (interval_secs),
            // sur le blocking pool. Changer pour les bytes bruts changerait
            // la définition du state_root → interdit après le 1er ancrage.
            let val_json = serde_json::to_vec(&uv).map_err(|e| format!("serialize utxo: {e}"))?;
            hasher.update(&val_json);
            hasher.update(b";");

            let amount = Decimal::from_str(&uv.amount).unwrap_or(Decimal::ZERO);
            *totals.entry(uv.asset_id.clone()).or_insert(Decimal::ZERO) += amount;
            count += 1;
        }

        producer
            .join()
            .map_err(|_| "reserves stream thread panicked".to_string())?
            .map_err(|e| format!("stream_all_utxos: {e}"))?;

        // Totaux triés (asset natif d'abord, puis ordre lexicographique) pour
        // un payload déterministe.
        let mut total_supply: Vec<(Option<String>, String)> = totals
            .into_iter()
            .map(|(asset, total)| (asset, total.to_string()))
            .collect();
        total_supply.sort();

        Ok(ReserveComputation {
            state_root: hex::encode(Sha256::digest(hasher.finalize())),
            total_supply,
            utxo_count: count,
        })
    })
    .await
    .map_err(|e| format!("compute_reserves join: {e}"))?
}

/// Calcule les réserves, ancre le bloc `ReserveSnapshot` dans le DAG et
/// enregistre le pointeur de commodité. Partagé entre la tâche périodique
/// (`spawn_reserve_snapshot_task`) et `POST /admin/reserves/snapshot`.
pub async fn perform_reserve_snapshot(state: &AppState) -> Result<Value, String> {
    let computation = compute_reserves(state.store.clone()).await?;
    let computed_at_ms = pms_core::utxo::current_time_ms();

    let payload = PayloadEnvelope::Plain(PlainPayload::ReserveSnapshot {
        state_root: computation.state_root.clone(),
        total_supply: computation.total_supply.clone(),
        utxo_count: computation.utxo_count,
        computed_at_ms,
    });

    let adapter = state.srv.adapter_arc();
    let parents = tx_helpers::get_block_parents(&state.store, &state.settings).await?;
    let wb = tx_helpers::forge_and_sign_block(
        Some(payload),
        parents,
        &adapter,
        &state.node_wallet,
        &state.settings,
        Some("Reserves: snapshot"),
    )
    .await?;

    match tx_helpers::persist_and_broadcast(state, &wb).await? {
        pms_storage::PutResult::Inserted => {}
        pms_storage::PutResult::Rejected(reason) => {
            return Err(format!("ReserveSnapshot block rejected: {reason}"));
        }
        other => return Err(format!("unexpected persist result: {other:?}")),
    }

    let snapshot = json!({
        "block_id": wb.id,
        "state_root": computation.state_root,
        "total_supply": computation.total_supply,
        "utxo_count": computation.utxo_count,
        "computed_at_ms": computed_at_ms,
    });

    // Pointeur de commodité (le bloc DAG reste la preuve).
    if let Err(e) = state.store.record_reserve_snapshot(&snapshot.to_string()) {
        tracing::warn!("failed to record reserve snapshot pointer: {e}");
    }

    tracing::info!(
        "🏦 Reserve snapshot anchored: block={} root={} utxos={}",
        &wb.id[..16.min(wb.id.len())],
        &computation.state_root[..16],
        computation.utxo_count
    );

    Ok(snapshot)
}

/// GET /v1/reserves/latest — dernier snapshot ancré (public, lecture seule).
pub async fn get_latest_reserves(State(state): State<AppState>) -> impl IntoResponse {
    match state.store.latest_reserve_snapshot() {
        Ok(Some(json_str)) => match serde_json::from_str::<Value>(&json_str) {
            Ok(v) => (StatusCode::OK, Json(v)),
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "corrupt reserve snapshot pointer"})),
            ),
        },
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no reserve snapshot anchored yet"})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("storage error: {e}")})),
        ),
    }
}

/// POST /admin/reserves/snapshot — déclenche un snapshot immédiat
/// (`admin_writable` : produit un bloc DAG, gated par read-only).
pub async fn admin_snapshot_reserves(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }
    match perform_reserve_snapshot(&state).await {
        Ok(snapshot) => (StatusCode::OK, Json(snapshot)),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        ),
    }
}

/// POST /admin/reserves/verify — recompute les réserves depuis l'état disque
/// et compare au dernier snapshot ancré (`admin_recovery` : lecture seule,
/// utilisable même en read-only mode pour diagnostiquer).
pub async fn admin_verify_reserves(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_authorized(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }

    let anchored: Value = match state.store.latest_reserve_snapshot() {
        Ok(Some(s)) => serde_json::from_str(&s).unwrap_or(Value::Null),
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "no reserve snapshot anchored yet"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("storage error: {e}")})),
            );
        }
    };

    let live = match compute_reserves(state.store.clone()).await {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e})),
            );
        }
    };

    // Le root vit : l'état évolue entre deux snapshots. Le verify confirme
    // que le RECALCUL est cohérent : root identique ⟺ état identique. Si
    // l'état a bougé depuis l'ancrage, on rapporte les deux pour diagnostic.
    let anchored_root = anchored
        .get("state_root")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let matches = anchored_root == live.state_root;

    (
        StatusCode::OK,
        Json(json!({
            "match": matches,
            "anchored": anchored,
            "live": {
                "state_root": live.state_root,
                "total_supply": live.total_supply,
                "utxo_count": live.utxo_count,
            },
            "note": if matches {
                "state unchanged since last anchor — root verified"
            } else {
                "state has changed since last anchor (normal under activity); compare totals"
            },
        })),
    )
}
