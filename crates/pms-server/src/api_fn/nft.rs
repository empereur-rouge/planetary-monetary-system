//! API endpoints pour les NFTs.
//!
//! Permet de query l'état des NFTs (ownership, existence).

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::api::AppState;

/// Réponse pour GET /v1/nft/{token_id}
#[derive(Debug, Serialize, Deserialize)]
pub struct NftResponse {
    /// Token ID demandé
    pub token_id: String,
    /// Propriétaire actuel (None si le token n'existe pas)
    pub owner: Option<String>,
    /// Le token existe-t-il ?
    pub exists: bool,
}

/// GET /v1/nft/{token_id}
///
/// Query l'ownership d'un NFT par son token_id.
///
/// # Réponses
/// - 200 OK : Token trouvé avec owner
/// - 404 Not Found : Token inexistant (exists: false)
/// - 500 Internal Server Error : Erreur de lecture store
pub async fn get_nft(
    State(state): State<AppState>,
    Path(token_id): Path<String>,
) -> impl IntoResponse {
    use pms_storage::NftStorage;

    // Accède au store directement depuis AppState
    let store = &state.store;

    match store.get_owner(&token_id) {
        Ok(Some(owner)) => {
            // Token existe avec un owner
            let response = NftResponse {
                token_id,
                owner: Some(owner),
                exists: true,
            };
            (StatusCode::OK, Json(response))
        }
        Ok(None) => {
            // Token n'existe pas
            let response = NftResponse {
                token_id,
                owner: None,
                exists: false,
            };
            (StatusCode::NOT_FOUND, Json(response))
        }
        Err(e) => {
            // Erreur interne
            tracing::error!("NFT get_owner error: {}", e);
            let response = NftResponse {
                token_id,
                owner: None,
                exists: false,
            };
            (StatusCode::INTERNAL_SERVER_ERROR, Json(response))
        }
    }
}
