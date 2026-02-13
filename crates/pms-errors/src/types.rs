use thiserror::Error;

/// Erreurs possibles lors de la validation d’un bloc.
#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("parent {0} missing")]
    ParentMissing(String),
    #[error("duplicate parents not allowed")]
    DuplicateParents,
    #[error("self-parent forbidden")]
    SelfParent,
    #[error("too many parents: {0}")]
    TooManyParents(String),
    #[error("input missing in block")]
    MissingInput,
    #[error("output missing in block")]
    MissingOutput,
    #[error("not enough parents")]
    NotEnoughParents,
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("too many inputs")]
    TooManyInputs,
    #[error("too many outputs")]
    TooManyOutputs,
    #[error("tx too large")]
    TxTooLarge,
    #[error("genesis rules violated")]
    BadGenesis,
    #[error("cycle détecté")]
    CycleDetected,
    #[error("genesis invalide: {0}")]
    InvalidGenesis(String),
    #[error("signature invalide: {0}")]
    InvalidSignature(String),
    #[error("double dépense")]
    DoubleSpend,
    #[error("fonds insuffisants")]
    InsufficientFunds,
    #[error("invalid amount: {reason}")]
    InvalidAmount { reason: String },
    #[error(
        "Invalid difficulty: {id} (expected at least 1 leading zero bit, got {required_bits} bits instead)"
    )]
    InvalidDifficulty { id: String, required_bits: u8 },
    // Mint
    #[error("unauthorized mint for block {id}, signer={signer_pk_hex}")]
    UnauthorizedMint { id: String, signer_pk_hex: String },
    #[error("mint amount too high for block {id}, max allowed={max_allowed}")]
    MintAmountTooHigh { id: String, max_allowed: String },
    // Fees
    #[error("Fee too high: {fee} > {max} (max_fee_per_tx in policy)")]
    FeeTooHigh { fee: String, max: String },
    #[error("Recipient {address} is not valid")]
    InvalidFeeRecipient { address: String },
    // Autres
    #[error("other: {0}")]
    Other(&'static str),

    // Configuration Security Errors
    #[error("invalid platform address signature: address '{address}' is not authorized by the master key")]
    InvalidPlatformSignature { address: String },

    #[error("missing platform address signature in config (required for Mainnet/Testnet)")]
    MissingPlatformSignature,

    #[error("cannot use {network} coordinator key in Dev mode - this is a critical security misconfiguration")]
    ProdKeyInDevMode { network: String },

    #[error("config error: {0}")]
    ConfigError(String),

    // Multi-token
    #[error("asset balance mismatch: asset={asset_id:?}, inputs={inputs}, outputs={outputs}")]
    AssetBalanceMismatch {
        asset_id: Option<String>,
        inputs: String,
        outputs: String,
    },
    #[error("token not registered: {0}")]
    TokenNotRegistered(String),
    #[error("unauthorized token mint: {0}")]
    UnauthorizedTokenMint(String),
    #[error("token max supply exceeded: {0}")]
    MaxSupplyExceeded(String),
    #[error("token already exists: {0}")]
    TokenAlreadyExists(String),

    // Compliance
    #[error("address is frozen: {0}")]
    AddressFrozen(String),
    #[error("reverse failed: output already spent: {txid}#{index}")]
    OutputAlreadySpent { txid: String, index: u32 },
}
