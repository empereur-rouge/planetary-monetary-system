//! Stable error codes for the PMS HTTP API (v0.7.24).
//!
//! Every error returned by an API handler should ultimately surface as
//! an `ApiError`. Each variant carries:
//!
//!   - **A stable numeric `code`** — part of the public wire contract.
//!     SDK clients branch on the code, not on the message string. Codes
//!     are 4-digit integers grouped by category:
//!     * `1xxx` — auth / authz
//!     * `2xxx` — request validation (specific publicly — no info leak)
//!     * `3xxx` — state / business logic (vague public)
//!     * `4xxx` — crypto / security (vague public — anti-enumeration)
//!     * `5xxx` — resource / quota (vague public)
//!     * `9xxx` — internal / unexpected
//!
//!   - **A vague public message** for state/auth/crypto categories so
//!     attackers can't enumerate engine state through error strings.
//!     ("Operation failed" vs "insufficient balance addr=ABC requested=10
//!     available=3"). Validation errors stay specific because they help
//!     legitimate clients without leaking sensitive details.
//!
//!   - **Internal context** (addresses, amounts, signature failure
//!     reasons) attached to the variant. Logged via `tracing::warn!` /
//!     `tracing::error!` with `target: "api_error"` and `code` field —
//!     visible to operators, never to clients.
//!
//! Adding a new code is **additive** (stable contract). Renumbering or
//! removing a code is a **breaking** change for SDK consumers — bump
//! `API_VERSION` in `api_fn::version` and document in CHANGELOG.
//!
//! # Wire format
//!
//! ```json
//! {
//!     "code": 3001,
//!     "message": "Operation failed"
//! }
//! ```
//!
//! Some variants add extra fields for SDK ergonomics — e.g. `ReadOnly`
//! preserves the legacy `error/reason/retry_after_seconds` fields and
//! attaches a `Retry-After: 30` header so existing clients keep working
//! through the migration.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

/// Every error category and variant in the public API. See module-
/// level docs for the numbering scheme.
#[derive(Debug, Clone)]
pub enum ApiError {
    // ── 1xxx auth / authz ──────────────────────────────────────────
    /// 1001 — `Authorization` / `X-Admin-Token` / `X-API-Key` header
    /// missing.
    MissingAuth,
    /// 1002 — Token / API key was supplied but didn't match.
    InvalidAuth,
    /// 1010 — Endpoint requires admin privileges; caller has API-key
    /// scope but no admin token.
    AdminRequired,
    /// 1020 — Engine is in read-only mode (resource pressure or
    /// operator-armed). Carries the reason for the SDK to branch on
    /// (memory / disk / rocksdb / manual).
    ReadOnly { reason: &'static str },
    /// 1030 — Caller IP not in the configured admin allowlist.
    IpNotAllowed { ip: String },
    /// 1040 — API key valid but lacks the scope required by the route.
    InsufficientScope {
        required: String,
        granted: Vec<String>,
    },

    // ── 2xxx request validation (specific public OK) ──────────────
    /// 2001 — JSON body malformed or didn't parse against the
    /// handler's schema.
    MalformedJson(String),
    /// 2010 — Address has wrong format / wrong HRP / bad bech32
    /// checksum.
    InvalidAddress { addr: String },
    /// 2020 — Amount out of range, negative, or wrong format.
    InvalidAmount { reason: String },
    /// 2030 — A required field is missing or has an invalid value.
    InvalidField {
        field: &'static str,
        reason: String,
    },
    /// 2040 — Body / parameter exceeded a configured size limit.
    TooLarge { kind: &'static str, limit: u64 },

    // ── 3xxx state / business (vague public) ──────────────────────
    /// 3001 — Source has insufficient balance for the requested
    /// transfer / fee / mint / burn. Internal log carries the full
    /// address + asset + requested + available.
    InsufficientBalance {
        addr: String,
        asset_id: Option<String>,
        requested: String,
        available: String,
    },
    /// 3010 — UTXO already spent (race vs another tx, or replay).
    AlreadySpent { txid: String, index: u32 },
    /// 3020 — Ledger ID does not exist in the registry.
    UnknownLedger(String),
    /// 3030 — Contract exists but is currently disabled (kill switch).
    ContractDisabled(String),
    /// 3040 — Resource (NFT, block, tx, token, contract) not found.
    NotFound { kind: &'static str, id: String },
    /// 3050 — Resource (token registry, contract id, ledger id, etc.)
    /// already exists.
    AlreadyExists { kind: &'static str, id: String },
    /// 3060 — Address is frozen by compliance (freeze / seize order).
    AddressFrozen(String),
    /// 3070 — Operation conflict with concurrent state change.
    Conflict(String),

    // ── 4xxx crypto / security (vague public, anti-enumeration) ───
    /// 4001 — Signature verification failed. Internal logs which
    /// input / pubkey / curve. Public message is generic so attackers
    /// can't probe for valid pubkeys.
    SignatureMismatch { reason: String },
    /// 4002 — Replay detected (block id already persisted, nonce
    /// reuse, etc.).
    ReplayDetected { reason: String },
    /// 4010 — Encryption / decryption error (X25519 / AES-256-GCM
    /// envelope corruption, missing recipient key, etc.).
    CryptoFailure { reason: String },
    /// 4020 — Authorization signature on a privileged payload
    /// (e.g. coordinator-only mint, treasury wallet bind) didn't
    /// match the configured master key.
    AuthorizationSignatureInvalid { reason: String },

    // ── 5xxx resource / quota (vague public) ──────────────────────
    /// 5001 — Per-IP rate limit hit.
    RateLimited,
    /// 5010 — Per-ledger gas pool empty (subscription / non-main
    /// ledger). Public says "Operation unavailable", logs say which
    /// ledger.
    GasPoolEmpty(String),
    /// 5020 — Ledger subscription expired or never bought.
    SubscriptionInactive(String),

    // ── 9xxx internal (vague public always) ───────────────────────
    /// 9001 — RocksDB / persist pipeline error.
    StorageError { reason: String },
    /// 9002 — DAG / consensus engine refused to integrate the block.
    ConsensusError { reason: String },
    /// 9999 — Catch-all for unexpected errors. Should be rare —
    /// tracking these in the metric tells us which handler still
    /// returns generic anyhow errors (migration target).
    Internal { reason: String },
}

impl ApiError {
    /// Stable numeric code — part of the public wire contract.
    pub fn code(&self) -> u32 {
        match self {
            ApiError::MissingAuth => 1001,
            ApiError::InvalidAuth => 1002,
            ApiError::AdminRequired => 1010,
            ApiError::ReadOnly { .. } => 1020,
            ApiError::IpNotAllowed { .. } => 1030,
            ApiError::InsufficientScope { .. } => 1040,
            ApiError::MalformedJson(_) => 2001,
            ApiError::InvalidAddress { .. } => 2010,
            ApiError::InvalidAmount { .. } => 2020,
            ApiError::InvalidField { .. } => 2030,
            ApiError::TooLarge { .. } => 2040,
            ApiError::InsufficientBalance { .. } => 3001,
            ApiError::AlreadySpent { .. } => 3010,
            ApiError::UnknownLedger(_) => 3020,
            ApiError::ContractDisabled(_) => 3030,
            ApiError::NotFound { .. } => 3040,
            ApiError::AlreadyExists { .. } => 3050,
            ApiError::AddressFrozen(_) => 3060,
            ApiError::Conflict(_) => 3070,
            ApiError::SignatureMismatch { .. } => 4001,
            ApiError::ReplayDetected { .. } => 4002,
            ApiError::CryptoFailure { .. } => 4010,
            ApiError::AuthorizationSignatureInvalid { .. } => 4020,
            ApiError::RateLimited => 5001,
            ApiError::GasPoolEmpty(_) => 5010,
            ApiError::SubscriptionInactive(_) => 5020,
            ApiError::StorageError { .. } => 9001,
            ApiError::ConsensusError { .. } => 9002,
            ApiError::Internal { .. } => 9999,
        }
    }

    /// HTTP status code paired with this error.
    pub fn http_status(&self) -> StatusCode {
        match self {
            ApiError::MissingAuth | ApiError::InvalidAuth => StatusCode::UNAUTHORIZED,
            ApiError::AdminRequired
            | ApiError::IpNotAllowed { .. }
            | ApiError::InsufficientScope { .. } => StatusCode::FORBIDDEN,
            ApiError::ReadOnly { .. }
            | ApiError::GasPoolEmpty(_)
            | ApiError::SubscriptionInactive(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::MalformedJson(_)
            | ApiError::InvalidAddress { .. }
            | ApiError::InvalidAmount { .. }
            | ApiError::InvalidField { .. }
            | ApiError::CryptoFailure { .. } => StatusCode::BAD_REQUEST,
            ApiError::TooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::InsufficientBalance { .. }
            | ApiError::AlreadySpent { .. }
            | ApiError::ContractDisabled(_)
            | ApiError::AddressFrozen(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::UnknownLedger(_) | ApiError::NotFound { .. } => StatusCode::NOT_FOUND,
            ApiError::AlreadyExists { .. } | ApiError::Conflict(_) => StatusCode::CONFLICT,
            // Crypto failures map to 401 instead of 400 to make timing
            // attacks against signature verification harder — same
            // status as a missing auth header so an attacker can't tell
            // "I passed the format check, just my sig was wrong" apart.
            ApiError::SignatureMismatch { .. }
            | ApiError::ReplayDetected { .. }
            | ApiError::AuthorizationSignatureInvalid { .. } => StatusCode::UNAUTHORIZED,
            ApiError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ApiError::StorageError { .. }
            | ApiError::ConsensusError { .. }
            | ApiError::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Public-facing message. **Vague** for state/auth/crypto/internal
    /// errors so attackers can't enumerate engine state through error
    /// strings; specific for validation errors where being explicit is
    /// useful and not a security risk.
    pub fn public_message(&self) -> String {
        match self {
            // Auth — vague (don't tell the attacker WHICH check failed).
            ApiError::MissingAuth => "Authentication required".into(),
            ApiError::InvalidAuth => "Authentication failed".into(),
            ApiError::AdminRequired => "Insufficient privileges".into(),
            ApiError::IpNotAllowed { .. } => "Insufficient privileges".into(),
            ApiError::InsufficientScope { required, .. } => {
                // Scope is OK to expose — the SDK needs to know which
                // scope to request, and listing required vs granted is
                // standard practice (Stripe does this).
                format!("Insufficient permissions (required: {})", required)
            }
            ApiError::ReadOnly { reason } => format!(
                "Service temporarily unavailable ({}): retry in 30s",
                reason
            ),

            // Validation — specific (helps legit clients, no info leak).
            ApiError::MalformedJson(reason) => format!("Malformed JSON: {}", reason),
            ApiError::InvalidAddress { .. } => "Invalid address format".into(),
            ApiError::InvalidAmount { reason } => format!("Invalid amount: {}", reason),
            ApiError::InvalidField { field, reason } => {
                format!("Invalid field '{}': {}", field, reason)
            }
            ApiError::TooLarge { kind, limit } => {
                format!("{} exceeds limit of {} bytes", kind, limit)
            }

            // State / business — vague.
            ApiError::InsufficientBalance { .. } => "Operation failed".into(),
            ApiError::AlreadySpent { .. } => "Operation failed".into(),
            ApiError::UnknownLedger(_) => "Resource not found".into(),
            ApiError::ContractDisabled(_) => "Operation unavailable".into(),
            ApiError::NotFound { kind, .. } => format!("{} not found", kind),
            ApiError::AlreadyExists { kind, .. } => format!("{} already exists", kind),
            ApiError::AddressFrozen(_) => "Operation forbidden".into(),
            ApiError::Conflict(_) => "Operation conflict".into(),

            // Crypto / security — always vague.
            ApiError::SignatureMismatch { .. } => "Authentication failed".into(),
            ApiError::ReplayDetected { .. } => "Authentication failed".into(),
            ApiError::CryptoFailure { .. } => "Invalid request".into(),
            ApiError::AuthorizationSignatureInvalid { .. } => "Authentication failed".into(),

            // Resource / quota.
            ApiError::RateLimited => "Rate limit exceeded".into(),
            ApiError::GasPoolEmpty(_) => "Service temporarily unavailable".into(),
            ApiError::SubscriptionInactive(_) => "Subscription required".into(),

            // Internal — never expose stack traces, RocksDB errors,
            // panics, etc. Operators see these in logs.
            ApiError::StorageError { .. } => "Internal error".into(),
            ApiError::ConsensusError { .. } => "Internal error".into(),
            ApiError::Internal { .. } => "Internal error".into(),
        }
    }

    /// Internal context for `tracing::warn!` / `tracing::error!`. Carries
    /// every field the variant has so the operator can debug from logs
    /// alone. Never sent to the client.
    pub fn internal_detail(&self) -> String {
        match self {
            ApiError::MissingAuth => "no Authorization / X-Admin-Token / X-API-Key header".into(),
            ApiError::InvalidAuth => "token comparison failed".into(),
            ApiError::AdminRequired => "user-scope token presented to admin route".into(),
            ApiError::IpNotAllowed { ip } => format!("IP not in admin allowlist: {}", ip),
            ApiError::InsufficientScope { required, granted } => format!(
                "insufficient scope: required={} granted={:?}",
                required, granted
            ),
            ApiError::ReadOnly { reason } => format!("read-only mode armed (reason: {})", reason),
            ApiError::MalformedJson(reason) => reason.clone(),
            ApiError::InvalidAddress { addr } => format!("address={}", addr),
            ApiError::InvalidAmount { reason } => reason.clone(),
            ApiError::InvalidField { field, reason } => {
                format!("field={} reason={}", field, reason)
            }
            ApiError::TooLarge { kind, limit } => format!("{} > {} bytes", kind, limit),
            ApiError::InsufficientBalance {
                addr,
                asset_id,
                requested,
                available,
            } => format!(
                "insufficient balance: addr={} asset={:?} requested={} available={}",
                addr, asset_id, requested, available
            ),
            ApiError::AlreadySpent { txid, index } => {
                format!("output already spent: {}#{}", txid, index)
            }
            ApiError::UnknownLedger(id) => format!("unknown ledger: {}", id),
            ApiError::ContractDisabled(id) => format!("contract disabled: {}", id),
            ApiError::NotFound { kind, id } => format!("{} not found: {}", kind, id),
            ApiError::AlreadyExists { kind, id } => format!("{} already exists: {}", kind, id),
            ApiError::AddressFrozen(addr) => format!("address frozen: {}", addr),
            ApiError::Conflict(reason) => reason.clone(),
            ApiError::SignatureMismatch { reason } => reason.clone(),
            ApiError::ReplayDetected { reason } => reason.clone(),
            ApiError::CryptoFailure { reason } => reason.clone(),
            ApiError::AuthorizationSignatureInvalid { reason } => reason.clone(),
            ApiError::RateLimited => "rate limit exceeded".into(),
            ApiError::GasPoolEmpty(id) => format!("gas pool empty for ledger: {}", id),
            ApiError::SubscriptionInactive(id) => format!("subscription inactive: {}", id),
            ApiError::StorageError { reason } => reason.clone(),
            ApiError::ConsensusError { reason } => reason.clone(),
            ApiError::Internal { reason } => reason.clone(),
        }
    }

    /// Extra fields appended to the public JSON body for SDK
    /// ergonomics — e.g. `reason: "manual"` on `ReadOnly` so SDK
    /// clients can distinguish memory pressure from operator
    /// maintenance without parsing the message string. Returns an
    /// empty object for variants that need no extras.
    fn public_extras(&self) -> Value {
        match self {
            // Read-only preserves the legacy fields so existing
            // clients (validated by the v0.7.23 sandbox test) keep
            // working through the migration to code-based handling.
            ApiError::ReadOnly { reason } => json!({
                "error": "read_only",
                "reason": reason,
                "retry_after_seconds": 30,
            }),
            ApiError::InsufficientScope { required, granted } => json!({
                "scope_required": required,
                "your_scopes": granted,
            }),
            _ => json!({}),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code(), self.internal_detail())
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = self.code();
        let status = self.http_status();
        let public_message = self.public_message();
        let detail = self.internal_detail();

        // Emit a metric increment (labelled by the code as a string —
        // bounded cardinality of ~25 codes total, safe for Prometheus).
        crate::metrics::API_ERRORS
            .with_label_values(&[&code.to_string()])
            .inc();

        // Route to warn / error based on severity. Internal errors
        // and crypto failures are loud; auth/validation are common
        // and stay at warn level.
        if status.is_server_error() || matches!(code, 4001 | 4002 | 4010 | 4020) {
            tracing::error!(
                target: "api_error",
                code,
                status = %status,
                detail = %detail,
            );
        } else {
            tracing::warn!(
                target: "api_error",
                code,
                status = %status,
                detail = %detail,
            );
        }

        // Build the JSON body. Always includes `code` and `message`;
        // some variants merge extra fields for legacy compat or SDK
        // ergonomics (see `public_extras`).
        let mut body = json!({
            "code": code,
            "message": public_message,
        });
        if let (Some(map), Value::Object(extras)) = (body.as_object_mut(), self.public_extras()) {
            for (k, v) in extras {
                map.insert(k, v);
            }
        }

        let mut response = (status, Json(body)).into_response();

        // ReadOnly attaches a Retry-After header so well-behaved HTTP
        // clients back off without parsing the body.
        if matches!(self, ApiError::ReadOnly { .. }) {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from_static("30"),
            );
        }

        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique() {
        // All variants — kept in sync manually but enumerated here
        // so a mistake (two variants returning the same code) is
        // caught by `cargo test`. If you add a new variant to ApiError,
        // add it here too.
        let all = [
            ApiError::MissingAuth.code(),
            ApiError::InvalidAuth.code(),
            ApiError::AdminRequired.code(),
            ApiError::ReadOnly { reason: "x" }.code(),
            ApiError::IpNotAllowed { ip: "x".into() }.code(),
            ApiError::InsufficientScope {
                required: "x".into(),
                granted: vec![],
            }
            .code(),
            ApiError::MalformedJson("x".into()).code(),
            ApiError::InvalidAddress { addr: "x".into() }.code(),
            ApiError::InvalidAmount { reason: "x".into() }.code(),
            ApiError::InvalidField {
                field: "x",
                reason: "y".into(),
            }
            .code(),
            ApiError::TooLarge {
                kind: "x",
                limit: 1,
            }
            .code(),
            ApiError::InsufficientBalance {
                addr: "x".into(),
                asset_id: None,
                requested: "1".into(),
                available: "0".into(),
            }
            .code(),
            ApiError::AlreadySpent {
                txid: "x".into(),
                index: 0,
            }
            .code(),
            ApiError::UnknownLedger("x".into()).code(),
            ApiError::ContractDisabled("x".into()).code(),
            ApiError::NotFound {
                kind: "x",
                id: "y".into(),
            }
            .code(),
            ApiError::AlreadyExists {
                kind: "x",
                id: "y".into(),
            }
            .code(),
            ApiError::AddressFrozen("x".into()).code(),
            ApiError::Conflict("x".into()).code(),
            ApiError::SignatureMismatch { reason: "x".into() }.code(),
            ApiError::ReplayDetected { reason: "x".into() }.code(),
            ApiError::CryptoFailure { reason: "x".into() }.code(),
            ApiError::AuthorizationSignatureInvalid { reason: "x".into() }.code(),
            ApiError::RateLimited.code(),
            ApiError::GasPoolEmpty("x".into()).code(),
            ApiError::SubscriptionInactive("x".into()).code(),
            ApiError::StorageError { reason: "x".into() }.code(),
            ApiError::ConsensusError { reason: "x".into() }.code(),
            ApiError::Internal { reason: "x".into() }.code(),
        ];
        let mut seen = std::collections::HashSet::new();
        for c in all {
            assert!(seen.insert(c), "duplicate code: {}", c);
        }
    }

    #[test]
    fn public_message_never_leaks_internal_detail() {
        // Variants that should be vague publicly — the internal
        // detail (address, amount, signature reason) MUST NOT appear
        // verbatim in the public message.
        let cases: Vec<ApiError> = vec![
            ApiError::InsufficientBalance {
                addr: "8e1secret_addr_xyz".into(),
                asset_id: Some("edenite".into()),
                requested: "999".into(),
                available: "1".into(),
            },
            ApiError::SignatureMismatch {
                reason: "ed25519 input[2] pubkey mismatch".into(),
            },
            ApiError::StorageError {
                reason: "rocksdb: l0 compaction stalled".into(),
            },
            ApiError::AddressFrozen("8e1frozen_addr".into()),
        ];
        for err in cases {
            let public = err.public_message();
            let detail = err.internal_detail();
            // The public message must NOT contain any substring of the
            // internal detail that would be sensitive (the addresses,
            // amounts, RocksDB error reasons, etc.).
            for token in detail.split_whitespace() {
                if token.len() < 4 {
                    continue; // skip noise like "=" / "addr"
                }
                assert!(
                    !public.contains(token),
                    "public message '{}' leaks internal token '{}' from detail '{}'",
                    public,
                    token,
                    detail
                );
            }
        }
    }
}
