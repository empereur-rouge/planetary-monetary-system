use crate::api::AppState;
use crate::api_fn::tx_helpers;
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::StatusCode;
use pms_contracts::engine::evaluate_transfer;
use pms_storage::{ConfigStorage, PutResult};
use pms_types::{Transaction, TxInput, TxOutput};
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
    pub x25519_sk_hex: String,
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
    let x25519_sk_hex = wallet.x25519_sk_hex().unwrap_or_default();

    (
        StatusCode::OK,
        Json(json!(WalletCreateResponse {
            address,
            private_key_b64: wallet.private_key_b64,
            private_key_hex,
            public_key_hex: wallet.public_key_hex,
            x25519_pub_hex: wallet.x25519_pub_hex,
            x25519_sk_hex,
            mnemonic_words: wallet.mnemonic_words,
        })),
    )
}

// ════════════════════════════════════════════════════════════════════════════
// POST /admin/wallet/restore/mnemonic — Restaure un wallet depuis 24 mots BIP39
//
// AUDIT H-5 (v0.9.1) : custodial-by-design. Le client transmet sa mnémonique
// (secret long-terme) dans le body et le serveur la renvoie + les clés
// dérivées. Réservé au middleware admin (`require_local_or_admin`) — voir
// routes.rs `admin_recovery`. La dérivation sans transmission du secret est
// préférable côté SDK ; cet endpoint existe pour les flux custodial assumés.
// NE JAMAIS logger le body de cette requête.
// ════════════════════════════════════════════════════════════════════════════

/// Corps de `POST /admin/wallet/restore/mnemonic`.
///
/// **Secret sensible** : `mnemonic` est une phrase BIP39 long-terme. Endpoint
/// admin-gated (audit H-5). Aucun logging du corps.
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
    pub x25519_sk_hex: String,
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
    let x25519_sk_hex = wallet.x25519_sk_hex().unwrap_or_default();
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
            x25519_sk_hex,
            mnemonic_words,
        })),
    )
}

// ════════════════════════════════════════════════════════════════════════════
// POST /admin/wallet/restore/private-key — Restaure un wallet depuis une clé privée hex
//
// AUDIT H-5 (v0.9.1) : custodial-by-design, admin-gated (voir restore/mnemonic).
// NE JAMAIS logger le body.
// ════════════════════════════════════════════════════════════════════════════

/// Corps de `POST /admin/wallet/restore/private-key`.
///
/// **Secret sensible** : `private_key_hex` est une clé privée ECDSA long-terme.
/// Endpoint admin-gated (audit H-5). Aucun logging du corps.
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
    let x25519_sk_hex = wallet.x25519_sk_hex().unwrap_or_default();

    (
        StatusCode::OK,
        Json(json!(WalletCreateResponse {
            address,
            private_key_b64: wallet.private_key_b64,
            private_key_hex: req.private_key_hex,
            public_key_hex: wallet.public_key_hex,
            x25519_pub_hex: wallet.x25519_pub_hex,
            x25519_sk_hex,
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
    /// Frais de gas PMS
    pub fee: String,
    /// Frais de transfert smart contract (dans le même asset que le transfert).
    /// `"0"` si aucun contrat de transfert n'est actif.
    pub transfer_fee: String,
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
    /// Time-lock optionnel (protocole 2.1, timestamp UNIX ms) : l'UTXO minté
    /// est indépensable avant cette échéance. Usages : vesting, constitution
    /// d'une réserve de collatéral (mint collatéralisé 2.3 v2).
    #[serde(default)]
    pub locked_until: Option<u64>,
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

    // 0.5) Kill-switch d'émission (gouvernance) — `mint_enabled = false` coupe
    // TOUTE émission de PMS natif, faucet inclus (c'est une voie de mint natif).
    // Cohérent avec le check dans `EmissionGate::reserve`.
    if let Ok(cfg) = state.store.get_runtime_config() {
        if !cfg.mint_enabled {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "native mint is disabled by governance" })),
            );
        }
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

    // 2) Build Mint payload (time-locké si demandé)
    let mint_output = match req.locked_until {
        Some(until) => TxOutput::new_locked(req.to.clone(), amount_dec.to_string(), None, until),
        None => TxOutput::new(req.to.clone(), amount_dec.to_string(), None),
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

    // 5) Persist + broadcast
    // NOTE: Do NOT call apply_utxo_delta here — PlainPayload::Mint is a plain payload,
    // so persist_block() already constructs the UtxoDelta and applies it via apply_diff().
    // Calling apply_utxo_delta again would double-count supply AND destroy the address
    // index (LRU re-insert evicts the existing entry, then addr_index_remove deletes the
    // outpoint that addr_index_add just re-added).
    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(PutResult::Inserted) => {
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
    // 0) Gas pool check (custom ledgers only)
    // ════════════════════════════════════════════════════════════════════
    if let Err(e) = crate::api_fn::tx_helpers::try_consume_gas(&state) {
        return (StatusCode::PAYMENT_REQUIRED, Json(json!({ "error": e })));
    }

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

    let mut fee_dec = fee_policy
        .compute_fee(&amount_dec.to_string())
        .map(|a| a.inner())
        .unwrap_or(Decimal::ZERO);

    // ════════════════════════════════════════════════════════════════════
    // 3.a) Évaluer les contrats de frais de transfert (smart contract fees)
    // ════════════════════════════════════════════════════════════════════
    let transfer_fees = evaluate_transfer(
        state.contract_store.as_ref(),
        &state.ledger_id,
        req.asset_id.as_deref(),
        amount_dec,
    );
    let total_transfer_fee: Decimal = transfer_fees.iter().map(|f| f.fee_amount).sum();

    let total_needed = if req.asset_id.is_some() {
        amount_dec + total_transfer_fee
    } else {
        amount_dec + fee_dec + total_transfer_fee
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

    // PMS inputs for fee (custom token transfers)
    // If sender has no PMS on this ledger, gracefully skip the protocol fee.
    // This solves the bootstrap problem on custom ledgers where PMS doesn't
    // exist yet (chicken-and-egg: need PMS to pay fees, need fees to mint PMS).
    // Smart contract transfer fees (in the custom asset) still apply.
    let mut pms_change = Decimal::ZERO;
    if req.asset_id.is_some() && fee_dec > Decimal::ZERO {
        match tx_helpers::select_utxos(&adapter, &from_address, fee_dec, &None).await {
            Ok((pms_selected, pms_sum)) => {
                for (output_id, _, _) in &pms_selected {
                    tx_inputs.push(TxInput {
                        out: output_id.clone(),
                    });
                }
                pms_change = pms_sum - fee_dec;
            }
            Err(_) => {
                // No PMS available — waive protocol fee for this custom-asset transfer.
                // The smart contract transfer fee (if configured) still provides
                // fee revenue to the ledger creator.
                tracing::info!(
                    "Custom asset transfer: no PMS available for protocol fee on ledger '{}', waiving fee",
                    state.ledger_id
                );
                fee_dec = Decimal::ZERO;
            }
        }
    }

    // ════════════════════════════════════════════════════════════════════
    // 6) Build outputs
    // ════════════════════════════════════════════════════════════════════
    let mut tx_outputs: Vec<TxOutput> = Vec::new();

    // Destination
    tx_outputs.push(TxOutput::new(req.to.clone(), amount_dec.to_string(), req.asset_id.clone()));

    // Transfer fee outputs (smart contract) — same asset as the transfer
    for fee_result in &transfer_fees {
        tx_outputs.push(TxOutput::new(fee_result.beneficiary_address.clone(), fee_result.fee_amount.to_string(), req.asset_id.clone()));
    }

    // Change
    let change = selected_sum - total_needed;
    if change > Decimal::ZERO {
        tx_outputs.push(TxOutput::new(from_address.clone(), change.to_string(), req.asset_id.clone()));
    }

    // Fee to admin (round-robin'd across coord shards when sharding is
    // enabled — see audit follow-up to v0.7.4 + AppState::fee_recipient_address).
    if fee_dec > Decimal::ZERO {
        let admin_addr = match state.fee_recipient_address() {
            Some(addr) => addr,
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "no admin wallet configured for fees" })),
                );
            }
        };

        tx_outputs.push(TxOutput::new(admin_addr, fee_dec.to_string(), None));
    }

    // PMS change (custom token)
    if pms_change > Decimal::ZERO {
        tx_outputs.push(TxOutput::new(from_address.clone(), pms_change.to_string(), None));
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

    let tx_hash = match unsigned_tx.signing_message(&state.settings.network.network_id) {
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
        unlocks: tx_helpers::replicate_unlocks(
            &sender_wallet.public_key_hex,
            &signature_b64,
            unsigned_tx.inputs.len(),
        ),
        ..unsigned_tx
    };

    // ════════════════════════════════════════════════════════════════════
    // 7.b) VALIDATION COMPLÈTE DU PLAINTEXT (audit 2026-06, cause A)
    // ════════════════════════════════════════════════════════════════════
    // Custodial, mais le payload est CHIFFRÉ → `persist_block` saute
    // `validate_transaction_full`. On valide donc le plaintext via la MÊME
    // fonction que le hot-path (gel compliance inputs/outputs, time-locks,
    // autorisation MultiSig/HashLock, dédup d'inputs, conservation) AVANT
    // chiffrement. Crucial : un émetteur GELÉ ne doit pas pouvoir dépenser via
    // cet endpoint (sinon bypass compliance).
    if let Err(e) = state
        .srv
        .adapter_arc()
        .validate_txutxo_full(&signed_tx, pms_utils::ts_ms())
        .await
    {
        tracing::warn!("wallet_send_simple: plaintext validation rejected: {e}");
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "transaction validation failed" })),
        );
    }

    // ════════════════════════════════════════════════════════════════════
    // 8) Encrypt payload
    // ════════════════════════════════════════════════════════════════════
    let mut recipients_xpk = vec![sender_wallet.x25519_pub_hex.clone()];

    // Auto-add the X25519 key of every coordinator fee recipient (shard, admin,
    // OR treasury) so it can decrypt its fee UTXO. Uses the SAME shared predicate
    // as the fee output selection (`fee_recipient_addresses`) — sinon, sous
    // sharding, le frais part vers une adresse de shard absente d'admin et sa clé
    // n'est jamais ajoutée (UTXO de frais indéchiffrable). Cohérent avec
    // `wallet_send_tx`.
    let fee_recipients = state.fee_recipient_addresses();
    for out in &signed_tx.outputs {
        if fee_recipients.contains(&out.address.to_ascii_lowercase()) {
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

    // Add transfer fee beneficiary X25519 keys
    for fee_result in &transfer_fees {
        if let Ok((_h20, xpk)) = pms_wallet::decode_address(&fee_result.beneficiary_address) {
            if !recipients_xpk.contains(&xpk) {
                recipients_xpk.push(xpk);
            }
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
    // Resolve sender BEFORE the persist call — once the delta is applied
    // atomically inside `persist_block_with_delta`, `get_utxo` on the
    // inputs returns `None` because they've been consumed (H1).
    let sender_addr = if let pms_types_payload::PlainPayload::TxUtxo(ref tx) = plain {
        if let Some(first_input) = tx.inputs.first() {
            state
                .srv
                .adapter_arc()
                .get_utxo(&first_input.out)
                .await
                .map(|u| u.address)
        } else {
            None
        }
    } else {
        None
    };

    match tx_helpers::persist_and_broadcast_with_delta(
        &state,
        &wb,
        &signed_tx.inputs,
        &signed_tx.outputs,
    )
    .await
    {
        Ok(PutResult::Inserted) => {

            // Index activity for encrypted payload (both untyped + typed + precomputed items).
            {
                let mut addrs = pms_storage::helpers::extract_involved_addresses(&plain);
                let mut typed = pms_storage::helpers::extract_involved_with_category(&plain);
                // Add sender to indexed addresses if not already present
                if let Some(ref sa) = sender_addr {
                    if !addrs.contains(sa) {
                        addrs.push(sa.clone());
                        typed.push((sa.clone(), pms_storage::helpers::ActivityCategory::Transfer));
                    }
                }
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

            // Accumulate fee in pool for periodic consolidated distribution
            tx_helpers::accumulate_tx_fee(&state, fee_dec).await;

            (
                StatusCode::CREATED,
                Json(json!(SendSimpleResponse {
                    block_id: wb.id,
                    fee: fee_dec.to_string(),
                    transfer_fee: total_transfer_fee.to_string(),
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
