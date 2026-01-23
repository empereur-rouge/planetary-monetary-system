use crate::api::AppState;
use axum::Json;
use axum::extract::State;
use pms_config::RuntimeConfig;
use pms_storage::ConfigStorage;
use serde::Serialize;

#[derive(Serialize)]
pub struct NodePublicConfig {
    #[serde(flatten)]
    pub runtime: RuntimeConfig,
    pub fee_recipient: String,
}

/// Retourne la configuration runtime actuelle (frais, PoW, etc.)
pub async fn get_config(State(st): State<AppState>) -> Json<NodePublicConfig> {
    // Essayer de lire la config depuis le store, sinon utiliser défaut
    let config = st
        .store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());

    // Take first admin address or treasury as fee recipient
    let fee_recipient = st
        .treasury_wallets
        .first()
        .cloned()
        .or_else(|| st.settings.admin.wallet_addresses.first().cloned())
        .unwrap_or_else(|| st.node_wallet.get_address("8e"));

    Json(NodePublicConfig {
        runtime: config,
        fee_recipient,
    })
}
