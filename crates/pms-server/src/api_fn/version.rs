//! Endpoint pour les informations de version du noeud.

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use crate::api::AppState;

/// Version de l'API REST — à incrémenter à chaque modification des routes/formats.
pub const API_VERSION: u32 = 11;

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
        assert_eq!(parsed.api_version, API_VERSION);
    }
}
