use serde::{Deserialize, Serialize};

/// Wallet info returned by POST /v1/wallet/create
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletInfo {
    pub address: String,
    pub private_key_b64: String,
    pub public_key_hex: String,
    pub x25519_pub_hex: String,
}

/// Request for POST /v1/wallet/send-simple
#[derive(Debug, Serialize)]
pub struct SendSimpleRequest {
    pub private_key_b64: String,
    pub to: String,
    pub amount: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
}

/// Response from POST /v1/wallet/send-simple
#[derive(Debug, Deserialize)]
pub struct SendResponse {
    pub block_id: Option<String>,
    pub fee: Option<String>,
    pub error: Option<String>,
}

/// Response from POST /v1/balance
#[derive(Debug, Deserialize)]
pub struct BalanceResponse {
    pub balance: String,
}

/// Request for POST /v1/balance
#[derive(Debug, Serialize)]
pub struct BalanceRequest {
    pub address: String,
}

/// Response from GET /v1/supply
#[derive(Debug, Deserialize)]
pub struct SupplyResponse {
    pub circulating_supply: String,
    pub utxo_count: u64,
}

/// Request for POST /admin/faucet
#[derive(Debug, Serialize)]
pub struct FaucetRequest {
    pub to: String,
    pub amount: String,
}

/// Response from POST /admin/faucet
#[derive(Debug, Deserialize)]
pub struct FaucetResponse {
    pub block_id: Option<String>,
    pub amount: Option<String>,
    pub error: Option<String>,
}

/// Request for POST /v1/cube/claim
#[derive(Debug, Serialize)]
pub struct CubeClaimRequest {
    pub to: String,
}

/// Response from POST /v1/cube/claim
#[derive(Debug, Deserialize)]
pub struct CubeClaimResponse {
    pub block_id: Option<String>,
    pub amount: Option<String>,
    pub asset_id: Option<String>,
    pub error: Option<String>,
}

/// Request for POST /v1/cube/burn
#[derive(Debug, Serialize)]
pub struct CubeBurnRequest {
    pub private_key_b64: String,
    pub amount: String,
}

/// Response from POST /v1/cube/burn
#[derive(Debug, Deserialize)]
pub struct CubeBurnResponse {
    pub block_id: Option<String>,
    pub cubes_burned: Option<String>,
    pub pms_received: Option<String>,
    pub error: Option<String>,
}

/// Request for POST /v1/dag/tips
#[derive(Debug, Serialize)]
pub struct TipsRequest {
    pub limit: usize,
}
