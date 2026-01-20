//! API endpoint pour les informations du Coordinateur.
//!
//! Permet aux clients de récupérer les clés publiques du nœud
//! pour vérifier les signatures et chiffrer des données.

use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::api::AppState;
use pms_wallet::SignerBackend;

/// Réponse pour GET /v1/coordinator/info
#[derive(Debug, Serialize)]
pub struct CoordinatorInfoResponse {
    /// Ce nœud est-il le Coordinateur ?
    pub is_coordinator: bool,
    /// Clé publique secp256k1 (hex) - pour vérifier les signatures
    pub secp256k1_pubkey: String,
    /// Clé publique X25519 (hex) - pour le chiffrement
    pub x25519_pubkey: String,
}

/// GET /v1/coordinator/info
///
/// Retourne les clés publiques du nœud actuel.
/// Utile pour:
/// - Vérifier si ce nœud est le Coordinateur
/// - Récupérer la clé secp256k1 pour vérifier les signatures
/// - Récupérer la clé X25519 pour chiffrer des données destinées au Coordinator
pub async fn get_coordinator_info(
    State(st): State<AppState>,
) -> (StatusCode, Json<CoordinatorInfoResponse>) {
    let settings = st.settings.as_ref();
    let node_wallet = &st.node_wallet;

    // Vérifie si ce nœud est le Coordinateur
    let is_coordinator = if let Some(coord_pk) = &settings.validation.coordinator_public_key {
        node_wallet.encoded_public_key() == *coord_pk
    } else {
        true // Dev mode: pas de coordinateur défini
    };

    // Retourne les clés du coordinateur définies dans settings,
    // ou celles du nœud local en fallback (Dev mode sans config explicite)
    let (secp256k1, x25519) = match (
        &settings.validation.coordinator_public_key,
        &settings.validation.coordinator_x25519_public_key,
    ) {
        (Some(secp), Some(x25519)) => (secp.clone(), x25519.clone()),
        _ => (
            node_wallet.encoded_public_key(),
            node_wallet.x25519_pub_hex().to_string(),
        ),
    };

    let response = CoordinatorInfoResponse {
        is_coordinator,
        secp256k1_pubkey: secp256k1,
        x25519_pubkey: x25519,
    };

    (StatusCode::OK, Json(response))
}
