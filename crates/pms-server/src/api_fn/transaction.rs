use crate::api::AppState;
use crate::api_fn::tx_helpers;
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use http::StatusCode;
use pms_storage::PutResult;
use pms_types::{Transaction, TxInput, TxOutput};
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
// use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
pub struct WalletSendTxRequest {
    // Transaction déjà signée côté wallet (unlocks remplis)
    pub tx: Transaction,
    // X25519 pubkeys hex pour le chiffrement du payload (sender + dest)
    pub recipients_xpk: Vec<String>,
}

pub async fn wallet_send_tx(
    State(state): State<AppState>,
    Json(body): Json<WalletSendTxRequest>,
) -> impl IntoResponse {
    // ============================================================
    // 0) Use settings from AppState (configured at startup/test time)
    // ============================================================
    let settings = &*state.settings;

    // ============================================================
    // 1) Parse fee (string -> Decimal) et normalise
    // ============================================================
    let tx = body.tx;

    let fee_dec = match Decimal::from_str_exact(&tx.fee) {
        Ok(d) => d,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid tx.fee decimal" })),
            );
        }
    };

    if fee_dec < Decimal::ZERO {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "tx.fee must be >= 0" })),
        );
    }

    // ============================================================
    // 2) Validation des frais (Calcul strict)
    // ============================================================
    // On n'injecte PLUS rien (cela casserait la signature client).
    // On VÉRIFIE que le client a bien inclus l'output de frais vers un admin.

    // a) Charger la policy (Runtime Config - Dynamic)
    let (fee_policy, _ratio_dec) = tx_helpers::load_fee_policy(&state.store);

    // b) STRICT: Verify Inputs == Outputs (No implicit fees)
    //    We must fetch inputs to sum them up.
    //    FIX: Use adapter RAM cache (ShardedUtxoSet) instead of store (RocksDB)
    //    to match prepareTx behavior and avoid desync with async persistence.
    let adapter = state.srv.adapter_arc();
    let mut total_inputs = Decimal::ZERO;
    for input in &tx.inputs {
        let output_id = pms_types::OutputId {
            txid: input.out.txid.clone(),
            index: input.out.index,
        };
        match adapter.get_utxo(&output_id).await {
            Some(u) => {
                if let Ok(amt) = Decimal::from_str_exact(&u.amount) {
                    total_inputs += amt;
                } else {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": "invalid decimal in stored utxo" })),
                    );
                }
            }
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(
                        json!({ "error": format!("input utxo not found (double spend?): {}:{}", input.out.txid, input.out.index) }),
                    ),
                );
            }
        }
    }

    let mut total_outputs = Decimal::ZERO;
    for out in &tx.outputs {
        if let Ok(amt) = Decimal::from_str_exact(&out.amount) {
            total_outputs += amt;
        } else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid output amount decimal" })),
            );
        }
    }

    if total_inputs != total_outputs {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "implicit fees invalid: total inputs must equal total outputs (including fee output)",
                "inputs": total_inputs.to_string(),
                "outputs": total_outputs.to_string(),
            })),
        );
    }

    // c) Identifier Sender Address pour exclure le Change
    //    On récupère l'adresse du sender depuis le premier UTXO input.
    //    Cela évite les problèmes de format H20 vs adresse Bech32.
    let sender_address: Option<String> = if let Some(first_input) = tx.inputs.first() {
        let output_id = pms_types::OutputId {
            txid: first_input.out.txid.clone(),
            index: first_input.out.index,
        };
        adapter
            .get_utxo(&output_id)
            .await
            .map(|u| u.address.clone())
    } else {
        None
    };

    // c) Identifier les outputs de frais (vers un wallet admin ou treasury)
    //    On tolère n'importe quel admin ou treasury de la liste
    let mut provided_fee = Decimal::ZERO;
    let mut taxable_amount = Decimal::ZERO;

    for out in &tx.outputs {
        // 1. Check Admin or Treasury (Fee)
        let is_admin = settings
            .admin
            .wallet_addresses
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&out.address));

        let is_treasury = settings
            .fees
            .treasury_addresses
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&out.address));

        if is_admin || is_treasury {
            if let Ok(amt) = Decimal::from_str_exact(&out.amount) {
                provided_fee += amt;
            }
        }
        // 2. Check Sender (Change/Self) - compare address directly
        else {
            let is_sender = sender_address
                .as_ref()
                .map(|s| s.eq_ignore_ascii_case(&out.address))
                .unwrap_or(false);

            if !is_sender {
                if let Ok(amt) = Decimal::from_str_exact(&out.amount) {
                    taxable_amount += amt;
                }
            }
        }
    }

    // c) Calculer le fee attendu
    // compute_fee() retourne maintenant un Amount avec précision garantie à 8 décimales
    let expected_fee = fee_policy
        .compute_fee(&taxable_amount.to_string())
        .map(|a| a.inner()) // Convertir Amount -> Decimal
        .unwrap_or(Decimal::ZERO);
    let expected_fee_dec = expected_fee;

    // d) Vérifier (avec une petite tolérance epsilon si besoin, mais Decimal est précis)
    if provided_fee < expected_fee_dec {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "insufficient fees",
                "provided": provided_fee.to_string(),
                "expected": expected_fee_dec.to_string(),
                "taxable_amount": taxable_amount.to_string()
            })),
        );
    }

    // ============================================================
    // 3) Chiffrement (recipients_xpk de base + AUTO-ADD ADMINS)
    // ============================================================
    // Si des frais sont payés vers une adresse admin, on DOIT ajouter
    // la clé publique (X25519) de cet admin dans la liste des destinataires
    // pour qu'il puisse déchiffrer et voir l'UTXO (et donc son solde).
    let mut recipients_xpk = body.recipients_xpk.clone();

    // On parcourt les outputs pour repérer les adresses admin
    for out in &tx.outputs {
        // Vérifie si c'est une adresse admin connue
        if settings
            .admin
            .wallet_addresses
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&out.address))
        {
            // On décode l'adresse pour extraire la X25519 PubKey
            // Format Bech32 : (H20, XPK_Hex)
            if let Ok((_h20, xpk)) = pms_wallet::decode_address(&out.address) {
                // On l'ajoute si elle n'est pas déjà présente
                if !recipients_xpk.contains(&xpk) {
                    recipients_xpk.push(xpk);
                }
            }
        }
    }

    let plain = PlainPayload::TxUtxo(tx);
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

    // ============================================================
    // 4) Parents via store
    // ============================================================
    let parents = match tx_helpers::get_block_parents(&state.store, settings).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": e })),
            );
        }
    };

    // ============================================================
    // 5) Forge bloc + WireBlock + signature
    // ============================================================
    let adapter = state.srv.adapter_arc();
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

    // ============================================================
    // 6) Persist + UTXO delta + broadcast + reward
    // ============================================================
    match tx_helpers::persist_and_broadcast(&state, &wb).await {
        Ok(PutResult::Inserted) => {
            // Apply UTXO delta for encrypted payload
            if let PlainPayload::TxUtxo(ref tx) = plain {
                tx_helpers::apply_utxo_delta(&adapter, &wb.id, &tx.inputs, &tx.outputs).await;
            }

            // Index activity for encrypted payload
            // (Plain payloads are indexed automatically in append_block_atomic_with_utxo,
            //  but encrypted payloads need explicit indexing since the coordinator
            //  knows the plain payload before encryption.)
            {
                let addrs = pms_storage::helpers::extract_involved_addresses(&plain);
                if let Err(e) = state.store.write_addr_activity_entries(&wb.id, &addrs) {
                    tracing::warn!("addr_activity index for encrypted block: {e}");
                }
            }

            // Create reward block for fee distribution
            tx_helpers::create_reward_block(&state, fee_dec, &wb.id).await;

            (
                StatusCode::CREATED,
                Json(json!({ "id": wb.id, "status": "inserted" })),
            )
        }
        Ok(PutResult::AlreadyExists) => (
            StatusCode::CONFLICT,
            Json(json!({ "id": wb.id, "status": "duplicate" })),
        ),
        Ok(PutResult::Rejected(reason)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "status": "rejected", "reason": reason })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        ),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// POST /v1/tx/prepare - Prépare une transaction non-signée pour le client
// ════════════════════════════════════════════════════════════════════════════

/// Requête pour préparer une transaction.
/// Le client fournit les adresses et le montant, le serveur construit la TX.
#[derive(Debug, Deserialize)]
pub struct PrepareTxRequest {
    /// Adresse Bech32 de l'expéditeur
    pub from: String,
    /// Adresse Bech32 du destinataire
    pub to: String,
    /// Montant à envoyer (string décimale, ex: "100.5")
    pub amount: String,
    /// Asset ID (None = PMS natif, Some("edenite") = token custom)
    #[serde(default)]
    pub asset_id: Option<String>,
}

/// Détail d'un UTXO sélectionné comme input
#[derive(Debug, Serialize)]
pub struct UtxoDetail {
    pub txid: String,
    pub index: u32,
    pub amount: String,
}

/// Réponse contenant la transaction non-signée prête à être signée par le client.
#[derive(Debug, Serialize)]
pub struct PrepareTxResponse {
    /// Transaction non-signée (unlocks vides, à remplir par le client)
    pub unsigned_tx: Transaction,
    /// Hash SHA256 du message à signer (hex)
    /// Le client doit signer ce hash avec sa clé privée ECDSA
    pub tx_hash: String,
    /// Frais calculés (string décimale)
    pub fee: String,
    /// Détail des UTXOs sélectionnés comme inputs
    pub inputs_detail: Vec<UtxoDetail>,
}

/// POST /v1/tx/prepare
///
/// Prépare une transaction de transfert wallet-à-wallet.
/// Le serveur sélectionne les UTXOs, calcule les frais, et construit la TX.
/// Le client reçoit la TX non-signée et le hash à signer.
///
/// # Flow complet
/// 1. Client appelle POST /v1/tx/prepare avec {from, to, amount}
/// 2. Serveur retourne {unsigned_tx, tx_hash, fee}
/// 3. Client signe `tx_hash` avec sa clé privée ECDSA
/// 4. Client remplit `unsigned_tx.unlocks` avec sa signature
/// 5. Client appelle POST /wallet/tx/send avec la TX signée
pub async fn prepare_tx(
    State(state): State<AppState>,
    Json(req): Json<PrepareTxRequest>,
) -> impl IntoResponse {
    // ════════════════════════════════════════════════════════════════════════
    // 1) Parse et validation du montant demandé
    // ════════════════════════════════════════════════════════════════════════
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

    // ════════════════════════════════════════════════════════════════════════
    // 2) Charger la policy de frais depuis la config runtime
    // ════════════════════════════════════════════════════════════════════════
    let (fee_policy, _ratio_dec) = tx_helpers::load_fee_policy(&state.store);

    // Calcul des frais sur le montant envoyé (taxable = amount vers destination)
    // compute_fee() retourne un Amount arrondi à 8 décimales
    let fee_dec = fee_policy
        .compute_fee(&amount_dec.to_string())
        .map(|a| a.inner())
        .unwrap_or(Decimal::ZERO);

    // For PMS native: total_needed = amount + fee
    // For custom tokens: total_needed = amount only (fee is separate in PMS)
    let total_needed = if req.asset_id.is_some() {
        amount_dec // Custom token: only need the amount from token UTXOs
    } else {
        amount_dec + fee_dec // PMS: amount + fee from same pool
    };

    // ════════════════════════════════════════════════════════════════════════
    // 2.b) Compliance: check frozen addresses
    // ════════════════════════════════════════════════════════════════════════
    if state.store.is_frozen(&req.from).unwrap_or(false) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "sender address is frozen" })),
        );
    }
    if state.store.is_frozen(&req.to).unwrap_or(false) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "recipient address is frozen" })),
        );
    }

    // ════════════════════════════════════════════════════════════════════════
    // 3) Récupérer les UTXOs + Coin Selection
    // ════════════════════════════════════════════════════════════════════════
    let adapter = state.srv.adapter_arc();

    let (selected_inputs, selected_sum) =
        match tx_helpers::select_utxos(&adapter, &req.from, total_needed, &req.asset_id).await {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({ "error": e })),
                );
            }
        };

    // ════════════════════════════════════════════════════════════════════════
    // 5) Construire les inputs (TxInput)
    // ════════════════════════════════════════════════════════════════════════
    let mut tx_inputs: Vec<TxInput> = selected_inputs
        .iter()
        .map(|(output_id, _, _)| TxInput {
            out: output_id.clone(),
        })
        .collect();

    let mut inputs_detail: Vec<UtxoDetail> = selected_inputs
        .iter()
        .map(|(output_id, _, amt)| UtxoDetail {
            txid: output_id.txid.clone(),
            index: output_id.index,
            amount: amt.to_string(),
        })
        .collect();

    // 5b) For custom token transfers, also select PMS UTXOs for fee payment
    let mut pms_change = Decimal::ZERO;
    if req.asset_id.is_some() && fee_dec > Decimal::ZERO {
        let (pms_selected, pms_sum) =
            match tx_helpers::select_utxos(&adapter, &req.from, fee_dec, &None).await {
                Ok(r) => r,
                Err(e) => {
                    return (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(json!({ "error": format!("insufficient PMS for fee: {e}") })),
                    );
                }
            };
        for (output_id, _, amt) in &pms_selected {
            tx_inputs.push(TxInput { out: output_id.clone() });
            inputs_detail.push(UtxoDetail {
                txid: output_id.txid.clone(),
                index: output_id.index,
                amount: amt.to_string(),
            });
        }
        pms_change = pms_sum - fee_dec;
    }

    // ════════════════════════════════════════════════════════════════════════
    // 6) Construire les outputs
    //    - Output 1: destination (to, amount, asset_id)
    //    - Output 2: change vers sender (from, change, asset_id) [si > 0]
    //    - Output 3: frais vers admin (admin, fee, None=PMS)
    //    - Output 4: PMS change vers sender [si custom token + PMS change > 0]
    // ════════════════════════════════════════════════════════════════════════
    let mut tx_outputs: Vec<TxOutput> = Vec::new();

    // Output destination
    tx_outputs.push(TxOutput {
        address: req.to.clone(),
        amount: amount_dec.to_string(),
        asset_id: req.asset_id.clone(),
    });

    // Change (retour vers l'expéditeur) — same asset as the transfer
    let change = selected_sum - total_needed;
    if change > Decimal::ZERO {
        tx_outputs.push(TxOutput {
            address: req.from.clone(),
            amount: change.to_string(),
            asset_id: req.asset_id.clone(),
        });
    }

    // Output frais vers admin wallet (fallback: treasury/coordinator)
    if fee_dec > Decimal::ZERO {
        // Priority: 1. admin.wallet_addresses, 2. fees.treasury_addresses, 3. error (no valid recipient)
        let admin_addr = state
            .settings
            .admin
            .wallet_addresses
            .first()
            .cloned()
            .or_else(|| state.settings.fees.treasury_addresses.first().cloned());

        let admin_addr = match admin_addr {
            Some(addr) => addr,
            None => {
                tracing::warn!("prepareTx: No admin or treasury address configured for fees!");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "no admin wallet configured for fees" })),
                );
            }
        };

        tx_outputs.push(TxOutput {
            address: admin_addr,
            amount: fee_dec.to_string(),
            asset_id: None, // fees always PMS
        });
    }

    // PMS change (only for custom token transfers where we also spent PMS for fees)
    if pms_change > Decimal::ZERO {
        tx_outputs.push(TxOutput {
            address: req.from.clone(),
            amount: pms_change.to_string(),
            asset_id: None, // PMS change
        });
    }

    // ════════════════════════════════════════════════════════════════════════
    // 7) Construire la Transaction non-signée
    //    unlocks est vide - le client doit le remplir après avoir signé
    // ════════════════════════════════════════════════════════════════════════
    let unsigned_tx = Transaction {
        inputs: tx_inputs,
        outputs: tx_outputs,
        fee: fee_dec.to_string(),
        unlocks: vec![], // À remplir par le client
    };

    // ════════════════════════════════════════════════════════════════════════
    // 8) Calculer le hash à signer
    //    Le client signera ce hash avec sa clé privée ECDSA
    // ════════════════════════════════════════════════════════════════════════
    let tx_hash = match unsigned_tx.signing_message() {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("failed to compute tx hash: {}", e) })),
            );
        }
    };

    // ════════════════════════════════════════════════════════════════════════
    // 9) Retourner la réponse
    // ════════════════════════════════════════════════════════════════════════
    (
        StatusCode::OK,
        Json(json!(PrepareTxResponse {
            unsigned_tx,
            tx_hash,
            fee: fee_dec.to_string(),
            inputs_detail,
        })),
    )
}
