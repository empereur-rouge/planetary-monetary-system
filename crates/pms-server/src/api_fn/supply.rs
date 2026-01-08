// ═══════════════════════════════════════════════════════════════════════════════
// supply.rs - API endpoint pour le supply en circulation
// ═══════════════════════════════════════════════════════════════════════════════
//
// Cet endpoint retourne le nombre total de tokens en circulation.
// Il est important pour la transparence de la supply totale de la monnaie.
//
// GET /v1/supply -> { "circulating_supply": "...", "utxo_count": N }
// ═══════════════════════════════════════════════════════════════════════════════

use crate::api::AppState;
use axum::{Json, extract::State};
use serde::Serialize;

/// Réponse de l'endpoint /v1/supply
#[derive(Debug, Serialize)]
pub struct CirculatingSupplyResponse {
    /// Nombre total de tokens en circulation (somme des UTXOs non dépensés)
    pub circulating_supply: String,

    /// Nombre de UTXOs actifs (outputs non dépensés)
    pub utxo_count: u64,
}

/// GET /v1/supply - Retourne le supply circulant
///
/// Cette API est publique et ne nécessite pas d'authentification.
/// Elle interroge le ShardedUtxoSet en RAM pour calculer la somme
/// de tous les outputs non dépensés.
///
/// ## Exemple de réponse
/// ```json
/// {
///   "circulating_supply": "1000000.00",
///   "utxo_count": 12345
/// }
/// ```
pub async fn get_circulating_supply(
    State(state): State<AppState>,
) -> Json<CirculatingSupplyResponse> {
    // Accède au ShardedUtxoSet via l'adapter du serveur
    let adapter = state.srv.adapter_arc();

    // Calcule le supply via la méthode du trait
    let (total, count) = adapter.circulating_supply().await;

    Json(CirculatingSupplyResponse {
        circulating_supply: total.to_string(),
        utxo_count: count as u64,
    })
}
