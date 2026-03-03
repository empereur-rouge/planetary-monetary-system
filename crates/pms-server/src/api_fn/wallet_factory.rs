use crate::api::AppState;
use crate::api_fn::tx_helpers;
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::StatusCode;
use pms_storage::PutResult;
use pms_types::{Transaction, TxInput, TxOutput, Unlock};
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_wallet::SignerBackend;
use pms_wallet::Wallet;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ════════════════════════════════════════════════════════════════════════════
// POST /v1/wallet/create — Génère un nouveau wallet et retourne les clés
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct WalletCreateRequest {
    /// Import depuis une clé privée hex existante (optionnel)
    #[serde(default)]
    pub import_hex: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WalletCreateResponse {
    pub address: String,
    pub private_key_b64: String,
    pub private_key_hex: String,
    pub public_key_hex: String,
    pub x25519_pub_hex: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mnemonic_words: Option<Vec<String>>,
}

pub async fn wallet_create(
    State(state): State<AppState>,
    body: Option<Json<WalletCreateRequest>>,
) -> impl IntoResponse {
    let hrp = &state.settings.address.hrp;

    let wallet = if let Some(Json(req)) = body {
        if let Some(hex_key) = req.import_hex {
            match Wallet::from_hex(&hex_key) {
                Ok(w) => w,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid private key: {e}") })),
                    );
                }
            }
        } else {
            Wallet::generate()
        }
    } else {
        Wallet::generate()
    };

    let address = wallet.get_address(hrp);
    let priv_bytes = STANDARD.decode(&wallet.private_key_b64).unwrap_or_default();
    let private_key_hex = hex::encode(&priv_bytes);

    (
        StatusCode::OK,
        Json(json!(WalletCreateResponse {
            address,
            private_key_b64: wallet.private_key_b64,
            private_key_hex,
            public_key_hex: wallet.public_key_hex,
            x25519_pub_hex: wallet.x25519_pub_hex,
            mnemonic_words: wallet.mnemonic_words,
        })),
    )
}

// ════════════════════════════════════════════════════════════════════════════
// POST /v1/wallet/restore/mnemonic — Restaure un wallet depuis 24 mots BIP39
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct RestoreMnemonicRequest {
    /// 24 mots BIP39 séparés par des espaces
    pub mnemonic: String,
}

#[derive(Debug, Serialize)]
pub struct RestoreMnemonicResponse {
    pub address: String,
    pub private_key_b64: String,
    pub private_key_hex: String,
    pub public_key_hex: String,
    pub x25519_pub_hex: String,
    pub mnemonic_words: Vec<String>,
}

pub async fn wallet_restore_mnemonic(
    State(state): State<AppState>,
    Json(req): Json<RestoreMnemonicRequest>,
) -> impl IntoResponse {
    let hrp = &state.settings.address.hrp;
    let words: Vec<&str> = req.mnemonic.split_whitespace().collect();

    let wallet = match Wallet::from_word_list(&words) {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("invalid mnemonic: {e}") })),
            );
        }
    };

    let address = wallet.get_address(hrp);
    let priv_bytes = STANDARD.decode(&wallet.private_key_b64).unwrap_or_default();
    let private_key_hex = hex::encode(&priv_bytes);
    let mnemonic_words = wallet
        .mnemonic_words
        .clone()
        .unwrap_or_else(|| words.iter().map(|w| w.to_string()).collect());

    (
        StatusCode::OK,
        Json(json!(RestoreMnemonicResponse {
            address,
            private_key_b64: wallet.private_key_b64,
            private_key_hex,
            public_key_hex: wallet.public_key_hex,
            x25519_pub_hex: wallet.x25519_pub_hex,
            mnemonic_words,
        })),
    )
}

// ════════════════════════════════════════════════════════════════════════════
// POST /v1/wallet/restore/private-key — Restaure un wallet depuis une clé privée hex
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct RestorePrivateKeyRequest {
    /// Clé privée hexadécimale (64 chars = 32 bytes)
    pub private_key_hex: String,
}

pub async fn wallet_restore_private_key(
    State(state): State<AppState>,
    Json(req): Json<RestorePrivateKeyRequest>,
) -> impl IntoResponse {
    let hrp = &state.settings.address.hrp;

    let wallet = match Wallet::from_hex(&req.private_key_hex) {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("invalid private key: {e}") })),
            );
        }
    };

    let address = wallet.get_address(hrp);

    (
        StatusCode::OK,
        Json(json!(WalletCreateResponse {
            address,
            private_key_b64: wallet.private_key_b64,
            private_key_hex: req.private_key_hex,
            public_key_hex: wallet.public_key_hex,
            x25519_pub_hex: wallet.x25519_pub_hex,
            mnemonic_words: None,
        })),
    )
}

// ════════════════════════════════════════════════════════════════════════════
// POST /v1/wallet/send-simple — One-shot custodial send (prepare + sign + send)
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct SendSimpleRequest {
    /// Clé privée base64 de l'expéditeur
    pub private_key_b64: String,
    /// Adresse Bech32 du destinataire
    pub to: String,
    /// Montant à envoyer (string décimale)
    pub amount: String,
    /// Asset ID (None = PMS natif)
    #[serde(default)]
    pub asset_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SendSimpleResponse {
    pub block_id: String,
    pub fee: String,
}

// ════════════════════════════════════════════════════════════════════════════
// POST /admin/faucet — Mint native PMS to an address (testnet/dev faucet)
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct FaucetRequest {
    /// Adresse Bech32 du destinataire
    pub to: String,
    /// Montant à minter (string décimale)
    pub amount: String,
}

#[derive(Debug, Serialize)]
pub struct FaucetResponse {
    pub block_id: String,
    pub amount: String,
}

pub async fn faucet_mint(
    State(state): State<AppState>,
    Json(req): Json<FaucetRequest>,
) -> impl IntoResponse {
    // 0) Dev/testnet only — reject on mainnet
    if state._cfg.network.mode.is_prod() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "faucet is disabled on mainnet" })),
        );
    }

    // 1) Parse amount
    let amount_dec = match Decimal::from_str_exact(&req.amount) {
        Ok(d) if d > Decimal::ZERO => d,
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "amount must be > 0" })),
            );
        }
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid amount decimal format" })),
            );
        }
    };

    // 2) Build Mint payload
    let mint_output = TxOutput {
        address: req.to.clone(),
        amount: amount_dec.to_string(),
        asset_id: None, // Native PMS
    };

    let mint_payload = PlainPayload::Mint {
        outputs: vec![mint_output.clone()],
    };
    let payload = Some(PayloadEnvelope::Plain(mint_payload));

    // 3) Get parents
    let settings = &*state.settings;
    let parents = match tx_helpers::get_block_parents(&state.store, settings).await {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": e })));
        }
    };

    // 4) Forge block + PoW + sign
    let adapter = state.srv.adapter_arc();
    let wb = match tx_helpers::forge_and_sign_block(
        payload,
        parents,
        &adapter,
        &state.node_wallet,
        settings,
        Some("Faucet mint"),
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e })),
            );
        }
    };

    // 5) Persist + broadcast + UTXO
    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(PutResult::Inserted) => {
            tx_helpers::apply_utxo_delta(&adapter, &wb.id, &[], &[mint_output]).await;
            (
                StatusCode::CREATED,
                Json(json!(FaucetResponse {
                    block_id: wb.id,
                    amount: amount_dec.to_string(),
                })),
            )
        }
        Ok(PutResult::AlreadyExists) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "block already exists" })),
        ),
        Ok(PutResult::Rejected(reason)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("rejected: {reason}") })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        ),
    }
}

/// Reconstruit un Wallet depuis private_key_b64
pub fn wallet_from_b64(priv_b64: &str) -> Result<Wallet, String> {
    let priv_bytes = STANDARD
        .decode(priv_b64)
        .map_err(|e| format!("invalid base64: {e}"))?;
    let priv_hex = hex::encode(&priv_bytes);
    Wallet::from_hex(&priv_hex)
}

pub async fn wallet_send_simple(
    State(state): State<AppState>,
    Json(req): Json<SendSimpleRequest>,
) -> impl IntoResponse {
    // ════════════════════════════════════════════════════════════════════
    // 1) Reconstruire le wallet depuis la clé privée
    // ════════════════════════════════════════════════════════════════════
    let sender_wallet = match wallet_from_b64(&req.private_key_b64) {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("invalid wallet key: {e}") })),
            );
        }
    };

    let hrp = &state.settings.address.hrp;
    let from_address = sender_wallet.get_address(hrp);

    // ════════════════════════════════════════════════════════════════════
    // 2) Parse et validation du montant
    // ════════════════════════════════════════════════════════════════════
    let amount_dec = match Decimal::from_str_exact(&req.amount) {
        Ok(d) if d > Decimal::ZERO => d,
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "amount must be > 0" })),
            );
        }
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid amount decimal format" })),
            );
        }
    };

    // ════════════════════════════════════════════════════════════════════
    // 3) Charger la policy de frais
    // ════════════════════════════════════════════════════════════════════
    let settings = &*state.settings;

    let (fee_policy, _ratio_dec) = tx_helpers::load_fee_policy(&state.store);

    let fee_dec = fee_policy
        .compute_fee(&amount_dec.to_string())
        .map(|a| a.inner())
        .unwrap_or(Decimal::ZERO);

    let total_needed = if req.asset_id.is_some() {
        amount_dec
    } else {
        amount_dec + fee_dec
    };

    // ════════════════════════════════════════════════════════════════════
    // 4) Récupérer les UTXOs + Coin Selection
    // ════════════════════════════════════════════════════════════════════
    let adapter = state.srv.adapter_arc();

    let (selected_inputs, selected_sum) = match tx_helpers::select_utxos(
        &adapter,
        &from_address,
        total_needed,
        &req.asset_id,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": e })),
            );
        }
    };

    // ════════════════════════════════════════════════════════════════════
    // 5) Build inputs
    // ════════════════════════════════════════════════════════════════════
    let mut tx_inputs: Vec<TxInput> = selected_inputs
        .iter()
        .map(|(output_id, _, _)| TxInput {
            out: output_id.clone(),
        })
        .collect();

    // PMS inputs for fee (custom token)
    let mut pms_change = Decimal::ZERO;
    if req.asset_id.is_some() && fee_dec > Decimal::ZERO {
        let (pms_selected, pms_sum) =
            match tx_helpers::select_utxos(&adapter, &from_address, fee_dec, &None).await {
                Ok(r) => r,
                Err(e) => {
                    return (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(json!({ "error": format!("insufficient PMS for fee: {e}") })),
                    );
                }
            };
        for (output_id, _, _) in &pms_selected {
            tx_inputs.push(TxInput {
                out: output_id.clone(),
            });
        }
        pms_change = pms_sum - fee_dec;
    }

    // ════════════════════════════════════════════════════════════════════
    // 6) Build outputs
    // ════════════════════════════════════════════════════════════════════
    let mut tx_outputs: Vec<TxOutput> = Vec::new();

    // Destination
    tx_outputs.push(TxOutput {
        address: req.to.clone(),
        amount: amount_dec.to_string(),
        asset_id: req.asset_id.clone(),
    });

    // Change
    let change = selected_sum - total_needed;
    if change > Decimal::ZERO {
        tx_outputs.push(TxOutput {
            address: from_address.clone(),
            amount: change.to_string(),
            asset_id: req.asset_id.clone(),
        });
    }

    // Fee to admin
    if fee_dec > Decimal::ZERO {
        let admin_addr = settings
            .admin
            .wallet_addresses
            .first()
            .cloned()
            .or_else(|| settings.fees.treasury_addresses.first().cloned());

        let admin_addr = match admin_addr {
            Some(addr) => addr,
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "no admin wallet configured for fees" })),
                );
            }
        };

        tx_outputs.push(TxOutput {
            address: admin_addr,
            amount: fee_dec.to_string(),
            asset_id: None,
        });
    }

    // PMS change (custom token)
    if pms_change > Decimal::ZERO {
        tx_outputs.push(TxOutput {
            address: from_address.clone(),
            amount: pms_change.to_string(),
            asset_id: None,
        });
    }

    // ════════════════════════════════════════════════════════════════════
    // 7) Build transaction + sign
    // ════════════════════════════════════════════════════════════════════
    let unsigned_tx = Transaction {
        inputs: tx_inputs,
        outputs: tx_outputs,
        fee: fee_dec.to_string(),
        unlocks: vec![],
    };

    let tx_hash = match unsigned_tx.signing_message() {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("tx hash failed: {e}") })),
            );
        }
    };

    let signature_b64 = match sender_wallet.sign(&tx_hash) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("signing failed: {e:?}") })),
            );
        }
    };

    let signed_tx = Transaction {
        unlocks: vec![Unlock {
            pubkey_hex: sender_wallet.public_key_hex.clone(),
            signature_b64,
        }],
        ..unsigned_tx
    };

    // ════════════════════════════════════════════════════════════════════
    // 8) Encrypt payload
    // ════════════════════════════════════════════════════════════════════
    let mut recipients_xpk = vec![sender_wallet.x25519_pub_hex.clone()];

    // Auto-add admin X25519 keys
    for out in &signed_tx.outputs {
        if settings
            .admin
            .wallet_addresses
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&out.address))
        {
            if let Ok((_h20, xpk)) = pms_wallet::decode_address(&out.address) {
                if !recipients_xpk.contains(&xpk) {
                    recipients_xpk.push(xpk);
                }
            }
        }
    }

    // Add destination X25519 key
    if let Ok((_h20, xpk)) = pms_wallet::decode_address(&req.to) {
        if !recipients_xpk.contains(&xpk) {
            recipients_xpk.push(xpk);
        }
    }

    let plain = PlainPayload::TxUtxo(signed_tx.clone());
    let enc = match EncryptedPayload::encrypt_for_plain(&plain, &recipients_xpk) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("encrypt failed: {e}") })),
            );
        }
    };
    let payload = Some(PayloadEnvelope::Encrypted(enc));

    // ════════════════════════════════════════════════════════════════════
    // 9) Get parents
    // ════════════════════════════════════════════════════════════════════
    let parents = match tx_helpers::get_block_parents(&state.store, settings).await {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": e })));
        }
    };

    // ════════════════════════════════════════════════════════════════════
    // 10) Forge block + PoW + sign
    // ════════════════════════════════════════════════════════════════════
    let wb = match tx_helpers::forge_and_sign_block(
        payload,
        parents,
        &adapter,
        &state.node_wallet,
        settings,
        None,
    )
    .await
    {
        Ok(wb) => wb,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e })),
            );
        }
    };

    // ════════════════════════════════════════════════════════════════════
    // 11) Persist + UTXO delta + broadcast + reward
    // ════════════════════════════════════════════════════════════════════
    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(PutResult::Inserted) => {
            tx_helpers::apply_utxo_delta(&adapter, &wb.id, &signed_tx.inputs, &signed_tx.outputs)
                .await;

            // Index activity for encrypted payload (both untyped + typed + precomputed items).
            {
                let addrs = pms_storage::helpers::extract_involved_addresses(&plain);
                let typed = pms_storage::helpers::extract_involved_with_category(&plain);
                let sender_addr = if let pms_types_payload::PlainPayload::TxUtxo(ref tx) = plain {
                    if let Some(first_input) = tx.inputs.first() {
                        state.srv.adapter_arc().get_utxo(&first_input.out).await.map(|u| u.address)
                    } else { None }
                } else { None };
                let precomputed = pms_storage::helpers::precompute_all_items(
                    &plain, &addrs, sender_addr.as_deref(),
                );
                if let Err(e) = state
                    .store
                    .write_addr_activity_entries_with_categories(&wb.id, &addrs, &typed, Some(&precomputed))
                {
                    tracing::warn!("addr_activity index for encrypted block: {e}");
                }
            }

            // Create reward block for fee distribution
            tx_helpers::create_reward_block(&state, fee_dec, &wb.id).await;

            (
                StatusCode::CREATED,
                Json(json!(SendSimpleResponse {
                    block_id: wb.id,
                    fee: fee_dec.to_string(),
                })),
            )
        }
        Ok(PutResult::AlreadyExists) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "block already exists" })),
        ),
        Ok(PutResult::Rejected(reason)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("rejected: {reason}") })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        ),
    }
}
