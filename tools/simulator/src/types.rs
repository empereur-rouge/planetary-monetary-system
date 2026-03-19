use serde::{Deserialize, Serialize};

/// Wallet info returned by POST /v1/wallet/create
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletInfo {
    pub address: String,
    pub private_key_b64: String,
    #[serde(default)]
    pub private_key_hex: String,
    pub public_key_hex: String,
    pub x25519_pub_hex: String,
    #[serde(default)]
    pub mnemonic_words: Option<Vec<String>>,
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

// ════════════════════════════════════════════════════════════════════════════
// Ledger Admin API
// ════════════════════════════════════════════════════════════════════════════

/// Request for POST /admin/ledgers/create
#[derive(Debug, Serialize)]
pub struct CreateLedgerRequest {
    pub id: String,
    pub network_id: String,
    pub prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_pubkey: Option<String>,
    /// Clé publique X25519 du propriétaire (pour chiffrement des blocs d'ownership transfer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_x25519_pubkey: Option<String>,
}

/// Response from POST /admin/ledgers/create
#[derive(Debug, Deserialize)]
pub struct CreateLedgerResponse {
    pub status: Option<String>,
    pub message: Option<String>,
}

// ════════════════════════════════════════════════════════════════════════════
// Token Admin API
// ════════════════════════════════════════════════════════════════════════════

/// Request for POST /admin/tokens/create
#[derive(Debug, Serialize)]
pub struct CreateTokenRequest {
    pub asset_id: String,
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_supply: Option<String>,
}

/// Response from POST /admin/tokens/create
#[derive(Debug, Deserialize)]
pub struct CreateTokenResponse {
    pub status: Option<String>,
}

/// Request for POST /admin/tokens/mint
#[derive(Debug, Serialize)]
pub struct MintTokenRequest {
    pub asset_id: String,
    pub to: String,
    pub amount: String,
}

/// Response from POST /admin/tokens/mint
#[derive(Debug, Deserialize)]
pub struct MintTokenResponse {
    pub status: Option<String>,
    pub block_id: Option<String>,
}

// ════════════════════════════════════════════════════════════════════════════
// NFT API
// ════════════════════════════════════════════════════════════════════════════

/// NFT metadata (simulator-side, mirrors pms-types-nft::NftMetadata)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NftMetadataSim {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nft_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<String>,
}

/// Request for POST /v1/nft/mint
#[derive(Debug, Serialize)]
pub struct MintNftRequest {
    pub token_id: String,
    pub owner_address: String,
    pub owner_x25519_pubkey: String,
    pub metadata: NftMetadataSim,
}

/// Response from POST /v1/nft/mint
#[derive(Debug, Deserialize)]
pub struct MintNftResponse {
    pub status: Option<String>,
    pub block_id: Option<String>,
}

/// Request for POST /v1/nft/burn-simple
#[derive(Debug, Serialize)]
pub struct BurnNftSimpleRequest {
    pub private_key_b64: String,
    pub token_id: String,
}

/// Response from POST /v1/nft/burn-simple
#[derive(Debug, Deserialize)]
pub struct BurnNftSimpleResponse {
    pub status: Option<String>,
    pub block_id: Option<String>,
    pub token_id: Option<String>,
}

/// Request for POST /v1/nft/burn-batch-simple
#[derive(Debug, Serialize)]
pub struct BurnNftBatchSimpleRequest {
    pub private_key_b64: String,
    pub token_ids: Vec<String>,
}

/// Response from POST /v1/nft/burn-batch-simple
#[derive(Debug, Deserialize)]
pub struct BurnNftBatchSimpleResponse {
    pub status: Option<String>,
    pub block_id: Option<String>,
    pub token_ids: Option<Vec<String>>,
}

/// Request for POST /v1/dag/tips
#[derive(Debug, Serialize)]
pub struct TipsRequest {
    pub limit: usize,
}

// ════════════════════════════════════════════════════════════════════════════
// Smart Contract API (mirrors pms-types-contract)
// ════════════════════════════════════════════════════════════════════════════

/// Request for POST /admin/contracts
#[derive(Debug, Serialize)]
pub struct RegisterContractRequest {
    pub name: String,
    pub scope: ContractScopeSim,
    pub trigger: ContractTriggerSim,
    pub actions: Vec<ContractActionSim>,
    pub enabled: bool,
}

/// Contract scope — which ledgers it applies to
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContractScopeSim {
    Global,
    Ledger(Vec<String>),
}

/// Contract trigger — what event fires the contract
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContractTriggerSim {
    OnNftBurn {
        nft_type: Option<String>,
    },
    OnTokenBurn {
        asset_id: String,
    },
    OnTransfer {
        asset_id: Option<String>,
    },
}

/// Contract action — what happens when the trigger fires
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContractActionSim {
    AccumulateRefund {
        asset_id: Option<String>,
        formula: MintFormulaSim,
    },
    EmitEvent {
        event_type: String,
    },
    TransferFee {
        formula: TransferFeeFormulaSim,
        splits: Vec<TransferFeeSplitSim>,
    },
}

/// Single split within a TransferFee action (simulator-side)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferFeeSplitSim {
    pub address: String,
    pub share_bps: u32,
}

/// Formula for calculating refund amounts
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MintFormulaSim {
    FixedRate {
        rate_numerator: u64,
        rate_denominator: u64,
    },
    AttributeFormula {
        attribute_names: Vec<String>,
        divisor: u64,
    },
    FixedAmount {
        amount: String,
    },
}

/// Formula for calculating transfer fees
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransferFeeFormulaSim {
    PercentageBps { rate_bps: u32 },
    FixedAmount { amount: String },
}

/// Response from POST /admin/contracts
#[derive(Debug, Deserialize)]
pub struct RegisterContractResponse {
    pub contract_id: Option<String>,
    pub status: Option<String>,
}

// ════════════════════════════════════════════════════════════════════════════
// UTXO Query API
// ════════════════════════════════════════════════════════════════════════════

/// Single UTXO entry from GET /v1/wallet/{address}/utxos.
///
/// Field names match the server's `UtxoFlatItem` (camelCase via `#[serde(rename)]`).
#[derive(Debug, Deserialize)]
pub struct UtxoEntry {
    #[serde(alias = "txId", alias = "txid")]
    pub txid: String,
    #[serde(alias = "outIdx", alias = "index")]
    pub index: u32,
    pub amount: String,
    #[serde(default)]
    pub asset_id: Option<String>,
}

/// Response from GET /v1/wallet/{address}/utxos
#[derive(Debug, Deserialize)]
pub struct UtxosResponse {
    pub utxos: Vec<UtxoEntry>,
}

// ════════════════════════════════════════════════════════════════════════════
// Gas Pool API
// ════════════════════════════════════════════════════════════════════════════

/// Request for POST /admin/gas-pool/deposit
#[derive(Debug, Serialize)]
pub struct GasPoolDepositRequest {
    pub ledger_id: String,
    pub amount: String,
}

/// Response from POST /admin/gas-pool/deposit
#[derive(Debug, Deserialize)]
pub struct GasPoolDepositResponse {
    pub status: Option<String>,
    pub ledger_id: Option<String>,
    pub deposited: Option<String>,
    pub new_balance: Option<String>,
}
