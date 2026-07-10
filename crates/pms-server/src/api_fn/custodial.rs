//! Provisionnement custodial d'assets **sans token admin** (protocole 2.8).
//!
//! Une instance creator-studio (custodiale : elle détient les clés de SES
//! créateurs) provisionne ses éditions capées + royalty en s'authentifiant par
//! la **clé du créateur**, pas par le token admin du DAG partagé. Le coordinateur
//! forge/signe les blocs (single-writer) mais NE PEUT PAS créer sous la
//! collection d'un autre ni minter la classe/le token d'un créateur sans sa clé.
//!
//! - `POST /v1/sft/classes`  — crée une classe SFT ; `creator = mint_authority =`
//!   adresse dérivée de `creator_private_key_b64`. (Le namespace collection est
//!   claimé au premier créateur au consensus — anti-squat.)
//! - `POST /v1/tokens/create` — idem pour un token fongible.
//! - `POST /v1/sft/mint` / `POST /v1/tokens/mint` — mint borné par `max_supply`,
//!   autorisé par la signature du `mint_authority` (clé custodiale OU pré-signée),
//!   forge un [`PlainPayload::CustodialMint`]. Le consensus RE-VÉRIFIE la signature
//!   + le cap + l'anti-replay (défense en profondeur).
//! - `POST /v1/sft/mint/prepare` / `POST /v1/tokens/mint/prepare` — renvoie le
//!   message canonique + le `mint_nonce` à signer (voie non-custodiale : le
//!   squelette signe localement, le coordinateur ne voit jamais la clé).
//!
//! Royalty : `royalty_bps`/`royalty_beneficiary` sont posés à la création et
//! **enforced au consensus** au `MarketSettle` (protocole 2.7) — indépendants du
//! chemin de mint.

use crate::api::AppState;
use crate::api_error::ApiError;
use crate::api_fn::market::{forge_persist_plain, resolve_meta};
use crate::api_fn::wallet_factory::wallet_from_b64;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use pms_types::{PlainPayload, SftClass, TokenMetadata, TxOutput};
use pms_wallet::SignerBackend;
use rand::Rng;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;

/// Nonce anti-replay aléatoire (32 octets hex) pour un mint custodial. Unique par
/// mint ; consommé une seule fois `(asset_id, mint_nonce)` au consensus.
fn random_mint_nonce() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

// ─────────────────────────── Create : classe SFT ───────────────────────────

/// `POST /v1/sft/classes` — crée une classe SFT custodiale. `creator` et
/// `mint_authority` sont **dérivés de `creator_private_key_b64`** (jamais fournis
/// en clair par le client → un opérateur ne peut pas usurper le `creator` d'un
/// autre, base de l'anti-squat de collection).
#[derive(Debug, Deserialize)]
pub struct CreateSftClassCustodialRequest {
    pub collection_id: String,
    pub class_id: String,
    pub name: String,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub attributes: Option<String>,
    #[serde(default)]
    pub decimals: u8,
    #[serde(default)]
    pub max_supply: Option<String>,
    #[serde(default)]
    pub demurrage_bps_per_day: Option<u32>,
    #[serde(default)]
    pub royalty_bps: Option<u32>,
    #[serde(default)]
    pub royalty_beneficiary: Option<String>,
    /// Clé privée base64 du CRÉATEUR (custodiée). `creator = mint_authority =`
    /// l'adresse dérivée. C'est cette clé qui autorisera les mints (Q1/Q2).
    pub creator_private_key_b64: String,
}

pub async fn create_sft_class_custodial(
    State(state): State<AppState>,
    Json(req): Json<CreateSftClassCustodialRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let wallet = wallet_from_b64(&req.creator_private_key_b64).map_err(|e| ApiError::InvalidField {
        field: "creator_private_key_b64",
        reason: e.to_string(),
    })?;
    let creator_addr = wallet.get_address(&state.settings.address.hrp);
    let asset_id = format!("{}:{}", req.collection_id, req.class_id);
    // La validation (format collection:class, unicité, royalty, claim de collection
    // anti-squat) vit au consensus (`SftClassCreate` arm). Handler = thin forge.
    let class = SftClass {
        asset_id: asset_id.clone(),
        collection_id: req.collection_id.clone(),
        class_id: req.class_id.clone(),
        name: req.name,
        uri: req.uri,
        attributes: req.attributes,
        decimals: req.decimals,
        max_supply: req.max_supply,
        demurrage_bps_per_day: req.demurrage_bps_per_day,
        creator: creator_addr.clone(),
        mint_authority: creator_addr.clone(),
        royalty_bps: req.royalty_bps.filter(|b| *b > 0),
        royalty_beneficiary: req.royalty_beneficiary,
        royalty_version: 0,
    };
    let block_id =
        forge_persist_plain(&state, PlainPayload::SftClassCreate(class), "SftClassCreateCustodial")
            .await?;
    Ok(Json(json!({
        "status": "ok",
        "asset_id": asset_id,
        "collection_id": req.collection_id,
        "class_id": req.class_id,
        "creator": creator_addr,
        "mint_authority": creator_addr,
        "block_id": block_id,
    })))
}

// ─────────────────────────── Create : token fongible ───────────────────────

/// `POST /v1/tokens/create` — crée un token fongible custodial. `creator` et
/// `mint_authority` dérivés de `creator_private_key_b64`.
#[derive(Debug, Deserialize)]
pub struct CreateTokenCustodialRequest {
    pub asset_id: String,
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    #[serde(default)]
    pub max_supply: Option<String>,
    #[serde(default)]
    pub demurrage_bps_per_day: Option<u32>,
    #[serde(default)]
    pub collateral_address: Option<String>,
    #[serde(default)]
    pub collateral_asset_id: Option<String>,
    #[serde(default)]
    pub collateral_ratio_bps: Option<u32>,
    #[serde(default)]
    pub royalty_bps: Option<u32>,
    #[serde(default)]
    pub royalty_beneficiary: Option<String>,
    pub creator_private_key_b64: String,
}

pub async fn create_token_custodial(
    State(state): State<AppState>,
    Json(req): Json<CreateTokenCustodialRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // `asset_id` token = `[a-z0-9_]{1,32}` — SANS `:` (garantit la disjonction de
    // namespace avec les classes SFT `collection:class`).
    if req.asset_id.is_empty()
        || req.asset_id.len() > 32
        || !req
            .asset_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(ApiError::InvalidField {
            field: "asset_id",
            reason: "must be 1-32 chars, lowercase alphanumeric or underscore".into(),
        });
    }
    if state
        .store
        .get_token(&req.asset_id)
        .map_err(|e| ApiError::StorageError { reason: e.to_string() })?
        .is_some()
    {
        return Err(ApiError::AlreadyExists {
            kind: "token",
            id: req.asset_id.clone(),
        });
    }
    let wallet = wallet_from_b64(&req.creator_private_key_b64).map_err(|e| ApiError::InvalidField {
        field: "creator_private_key_b64",
        reason: e.to_string(),
    })?;
    let creator_addr = wallet.get_address(&state.settings.address.hrp);
    let metadata = TokenMetadata {
        asset_id: req.asset_id.clone(),
        symbol: req.symbol,
        name: req.name,
        decimals: req.decimals,
        max_supply: req.max_supply,
        creator: creator_addr.clone(),
        mint_authority: creator_addr.clone(),
        demurrage_bps_per_day: req.demurrage_bps_per_day.filter(|b| *b > 0),
        collateral_address: req.collateral_address,
        collateral_asset_id: req.collateral_asset_id,
        collateral_ratio_bps: req.collateral_ratio_bps,
        royalty_bps: req.royalty_bps.filter(|b| *b > 0),
        royalty_beneficiary: req.royalty_beneficiary,
        royalty_version: 0,
    };
    // Le registre token n'est PAS reconstruit depuis le DAG (à la différence des
    // classes SFT) → on l'enregistre directement, puis on forge un bloc TokenCreate
    // (trace d'audit). Même séquence que le handler admin (`register_token` valide
    // les champs, dont collatéral).
    state
        .store
        .register_token(&metadata)
        .map_err(|e| ApiError::InvalidField {
            field: "token",
            reason: format!("registry rejected: {e}"),
        })?;
    let block_id = forge_persist_plain(
        &state,
        PlainPayload::TokenCreate(metadata.clone()),
        "TokenCreateCustodial",
    )
    .await?;
    Ok(Json(json!({
        "status": "ok",
        "asset_id": req.asset_id,
        "creator": creator_addr,
        "mint_authority": creator_addr,
        "block_id": block_id,
    })))
}

// ─────────────────────────── Mint (token OU SFT) ───────────────────────────

/// Requête de mint custodial. Autorisation : SOIT `mint_authority_private_key_b64`
/// (custodial : le serveur signe), SOIT `(auth_pubkey_hex, auth_signature_b64,
/// mint_nonce)` pré-signés (le coordinateur ne voit jamais la clé).
#[derive(Debug, Deserialize)]
pub struct CustodialMintRequest {
    /// `asset_id` du token OU `"collection:class"` de la classe SFT.
    pub asset_id: String,
    pub to: String,
    pub amount: String,
    /// Time-lock optionnel (UNIX ms) sur l'UTXO minté (protocole 2.1).
    #[serde(default)]
    pub locked_until: Option<u64>,
    #[serde(default)]
    pub mint_authority_private_key_b64: Option<String>,
    #[serde(default)]
    pub auth_pubkey_hex: Option<String>,
    #[serde(default)]
    pub auth_signature_b64: Option<String>,
    /// Nonce anti-replay. Requis en voie pré-signée (il fait partie du message
    /// signé). En voie custodiale, généré par le serveur si absent.
    #[serde(default)]
    pub mint_nonce: Option<String>,
}

/// Construit l'output minté (une seule sortie vers `to`). Partagé par mint + prepare
/// pour que le message signé soit identique des deux côtés.
fn build_mint_output(to: &str, amount: &Decimal, asset_id: &str, locked_until: Option<u64>) -> TxOutput {
    let mut out = TxOutput::new(to.to_string(), amount.to_string(), Some(asset_id.to_string()));
    out.locked_until = locked_until;
    out
}

fn parse_positive_amount(amount: &str) -> Result<Decimal, ApiError> {
    Decimal::from_str_exact(amount)
        .ok()
        .filter(|d| *d > Decimal::ZERO)
        .ok_or(ApiError::InvalidField {
            field: "amount",
            reason: "must be a positive decimal".into(),
        })
}

pub async fn custodial_mint(
    State(state): State<AppState>,
    Json(req): Json<CustodialMintRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let meta = resolve_meta(&state, &req.asset_id).ok_or_else(|| ApiError::NotFound {
        kind: "asset",
        id: req.asset_id.clone(),
    })?;
    let amount = parse_positive_amount(&req.amount)?;
    let outputs = vec![build_mint_output(&req.to, &amount, &req.asset_id, req.locked_until)];

    // Autorisation → (auth_pubkey_hex, auth_signature_b64, mint_nonce).
    let (auth_pubkey_hex, auth_signature_b64, mint_nonce) =
        if let Some(pk_b64) = &req.mint_authority_private_key_b64 {
            // Voie custodiale : reconstruit le wallet, vérifie qu'il EST le
            // mint_authority (403 sinon), génère le nonce si absent, signe.
            let wallet = wallet_from_b64(pk_b64).map_err(|e| ApiError::InvalidField {
                field: "mint_authority_private_key_b64",
                reason: e.to_string(),
            })?;
            if !pms_core::validations::ownership::unlock_matches_address(
                &wallet.public_key_hex,
                &meta.mint_authority,
            ) {
                return Err(ApiError::Forbidden {
                    reason: "provided key is not the asset mint_authority".into(),
                });
            }
            let nonce = req.mint_nonce.clone().unwrap_or_else(random_mint_nonce);
            let msg = pms_types::custodial_mint_signing_message(
                &state.settings.network.network_id,
                &req.asset_id,
                &outputs,
                &nonce,
            );
            let sig = wallet.sign(&msg).map_err(|e| ApiError::Internal {
                reason: format!("mint_authority sign failed: {e:?}"),
            })?;
            (wallet.public_key_hex.clone(), sig, nonce)
        } else if let (Some(pk), Some(sig), Some(nonce)) =
            (&req.auth_pubkey_hex, &req.auth_signature_b64, &req.mint_nonce)
        {
            // Voie pré-signée : le coordinateur ne voit jamais la clé. Persist vérifie.
            (pk.clone(), sig.clone(), nonce.clone())
        } else {
            return Err(ApiError::InvalidField {
                field: "authorization",
                reason: "provide mint_authority_private_key_b64 OR (auth_pubkey_hex + auth_signature_b64 + mint_nonce)".into(),
            });
        };

    let payload = PlainPayload::CustodialMint {
        asset_id: req.asset_id.clone(),
        outputs,
        auth_pubkey_hex,
        auth_signature_b64,
        mint_nonce: mint_nonce.clone(),
    };
    let block_id = forge_persist_plain(&state, payload, "CustodialMint").await?;
    Ok(Json(json!({
        "status": "ok",
        "asset_id": req.asset_id,
        "to": req.to,
        "amount": amount.to_string(),
        "mint_nonce": mint_nonce,
        "block_id": block_id,
    })))
}

/// Requête de prépa de mint : renvoie le message à signer + le nonce, sans rien
/// produire. Voie non-custodiale (le squelette signe localement).
#[derive(Debug, Deserialize)]
pub struct CustodialMintPrepareRequest {
    pub asset_id: String,
    pub to: String,
    pub amount: String,
    #[serde(default)]
    pub locked_until: Option<u64>,
    /// Nonce imposé (sinon généré). DOIT être réutilisé tel quel dans le mint.
    #[serde(default)]
    pub mint_nonce: Option<String>,
}

pub async fn custodial_mint_prepare(
    State(state): State<AppState>,
    Json(req): Json<CustodialMintPrepareRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let meta = resolve_meta(&state, &req.asset_id).ok_or_else(|| ApiError::NotFound {
        kind: "asset",
        id: req.asset_id.clone(),
    })?;
    let amount = parse_positive_amount(&req.amount)?;
    let outputs = vec![build_mint_output(&req.to, &amount, &req.asset_id, req.locked_until)];
    let mint_nonce = req.mint_nonce.clone().unwrap_or_else(random_mint_nonce);
    let message_hex = pms_types::custodial_mint_signing_message(
        &state.settings.network.network_id,
        &req.asset_id,
        &outputs,
        &mint_nonce,
    );
    Ok((
        StatusCode::OK,
        Json(json!({
            "asset_id": req.asset_id,
            "mint_authority": meta.mint_authority,
            "to": req.to,
            "amount": amount.to_string(),
            "locked_until": req.locked_until,
            "mint_nonce": mint_nonce,
            "message_hex": message_hex,
        })),
    ))
}
