//! Endpoint pour les informations de version du noeud.

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use crate::api::AppState;

/// Version de l'API REST — à incrémenter à chaque modification des routes/formats.
/// v18 (v0.14.0) : nouvelle route `POST /v1/wallet/token/burn` (burn de token
/// owner-signé, plan §3.1 voie B) + nouveau `PlainPayload::TokenBurn`
/// (DAG_VERSION 3.3.0).
/// v17 (v0.13.0) : nouvelle route `POST /admin/onramp` (voie A fiat→PMS, mint
/// natif sous budget d'émission partagé, plan §3.1) ; nouveau code d'erreur
/// `5030` (EmissionBudgetExhausted) sur les chemins de mint gatés.
/// v14 (audit H-5, v0.9.2) : les endpoints de restauration de wallet passent de
/// `/v1/wallet/restore/{mnemonic,private-key}` (API-key) à
/// `/admin/wallet/restore/{mnemonic,private-key}` (admin-gated). L'ancien
/// chemin renvoie 404.
/// v13 (audit sécurité v0.9.0) : `POST /v1/wallet/tx/send` exige des unlocks
/// valides (401 sinon) et la conservation par asset ; `POST /submit/block`
/// rejette les tx sans autorisation de dépense et les block ids non canoniques.
pub const API_VERSION: u32 = 18;

/// Réponse pour GET /v1/version
#[derive(Debug, Serialize, Deserialize)]
pub struct VersionResponse {
    /// Version du logiciel (Cargo.toml)
    pub software_version: String,
    /// Version du protocole DAG (SemVer, stockée dans RocksDB)
    pub dag_version: String,
    /// Version du schéma RocksDB (entier)
    pub schema_version: i64,
    /// Version du protocole P2P
    pub protocol_version: u32,
    /// Version de l'API REST (entier)
    pub api_version: u32,
}

/// GET /v1/version
///
/// Retourne toutes les informations de version du noeud.
pub async fn get_version(
    State(st): State<AppState>,
) -> (StatusCode, Json<VersionResponse>) {
    let software_version = env!("CARGO_PKG_VERSION").to_string();
    let dag_version = st
        .store
        .get_dag_version()
        .await
        .unwrap_or_else(|_| "1.0.0".to_string());
    let schema_version = st.store.get_version().await.unwrap_or(0);
    let protocol_version = st.settings.network.protocol_version;

    (
        StatusCode::OK,
        Json(VersionResponse {
            software_version,
            dag_version,
            schema_version,
            protocol_version,
            api_version: API_VERSION,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_response_serialization() {
        let response = VersionResponse {
            software_version: "0.1.3".to_string(),
            dag_version: "1.0.0".to_string(),
            schema_version: 5,
            protocol_version: 1,
            api_version: API_VERSION,
        };

        let json = serde_json::to_string_pretty(&response).unwrap();
        println!("Version API response JSON:\n{json}");

        let parsed: VersionResponse = serde_json::from_str(&json).unwrap();
        println!("Parsed back: software={}, dag={}, schema={}, protocol={}, api={}",
            parsed.software_version, parsed.dag_version,
            parsed.schema_version, parsed.protocol_version, parsed.api_version);

        assert_eq!(parsed.software_version, "0.1.3");
        assert_eq!(parsed.dag_version, "1.0.0");
        assert_eq!(parsed.schema_version, 5);
        assert_eq!(parsed.protocol_version, 1);
        // Pin the LITERAL (not `API_VERSION` vs itself): any bump of API_VERSION
        // must consciously update this assertion + the CHANGELOG. The real
        // GET /v1/version handler is exercised in tests/version_endpoint.rs.
        // v0.11.0: 15 → 16 (faucet locked_until + champs collateral_* sur
        // /admin/tokens/create — mint collatéralisé 2.3 v2).
        // v0.13.0: 16 → 17 (POST /admin/onramp voie A + code 5030).
        // v0.14.0: 17 → 18 (POST /v1/wallet/token/burn + PlainPayload::TokenBurn).
        assert_eq!(parsed.api_version, 18);
    }
}
