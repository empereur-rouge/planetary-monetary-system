use crate::api::AppState;
use crate::api_fn::tx_helpers;
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use http::StatusCode;
use pms_contracts::engine::evaluate_transfer;
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
    // 0a) Gas pool check (custom ledgers only)
    // ============================================================
    if let Err(e) = tx_helpers::try_consume_gas(&state) {
        return (StatusCode::PAYMENT_REQUIRED, Json(json!({ "error": e })));
    }

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
    // 1.b) VALIDATION COMPLÈTE DU PLAINTEXT (audit 2026-06, cause A)
    // ============================================================
    // Ce handler chiffre le payload : le hot path (`persist_block`) ne voit que
    // le ciphertext et SAUTE `validate_transaction_full`. Toute la validation
    // DOIT donc se faire ICI, sur le plaintext, avant chiffrement. On appelle la
    // MÊME fonction que le hot path (`adapter.validate_txutxo_full`), qui couvre :
    //   - appariement input/unlock + signatures ECDSA,
    //   - binding ownership C-1 + autorisation MultiSig/HashLock,
    //   - time-locks des inputs,
    //   - dédup des inputs dupliqués (anti-inflation : `[A,A]` rejeté),
    //   - conservation par-asset,
    //   - gel compliance (inputs + outputs).
    // Source unique → aucune dérive possible entre les deux chemins. Message
    // public volontairement vague (anti-enumeration) ; détail loggé.
    let adapter = state.srv.adapter_arc();
    let input_outputs = match adapter.validate_txutxo_full(&tx, pms_utils::ts_ms()).await {
        Ok(outs) => outs,
        Err(e) => {
            tracing::warn!("wallet_send_tx: plaintext validation rejected: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "transaction validation failed" })),
            );
        }
    };

    // ============================================================
    // 2) Validation des FRAIS (frais BRÛLÉ à la source : in − out)
    // ============================================================
    // Le frais de gas n'est plus un output vers le coordinateur : c'est
    // `Σ(inputs PMS) − Σ(outputs PMS)`, détruit au niveau de la tx (conservation
    // `out ≤ in`, phase 2a). On vérifie ici qu'il couvre le minimum attendu
    // (anti-spam / revenu). `taxable` = les outputs vers d'autres que le sender
    // (le change retourne au sender et n'est pas taxé).
    let (fee_policy, _ratio_dec) = tx_helpers::load_fee_policy(&state.store);
    let sender_address: Option<String> = input_outputs.first().map(|u| u.address.clone());

    let native_in: Decimal = input_outputs
        .iter()
        .filter(|u| u.asset_id.is_none())
        .filter_map(|u| Decimal::from_str_exact(&u.amount).ok())
        .sum();
    let native_out: Decimal = tx
        .outputs
        .iter()
        .filter(|o| o.asset_id.is_none())
        .filter_map(|o| Decimal::from_str_exact(&o.amount).ok())
        .sum();
    // ≥ 0 garanti par la conservation (`out ≤ in` pour le PMS natif).
    let burned_fee = native_in - native_out;

    let taxable_amount: Decimal = tx
        .outputs
        .iter()
        .filter(|o| {
            sender_address
                .as_ref()
                .map(|s| !s.eq_ignore_ascii_case(&o.address))
                .unwrap_or(true)
        })
        .filter_map(|o| Decimal::from_str_exact(&o.amount).ok())
        .sum();

    let expected_fee = fee_policy
        .compute_fee(&taxable_amount.to_string())
        .map(|a| a.inner())
        .unwrap_or(Decimal::ZERO);

    if burned_fee < expected_fee {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "insufficient fees",
                "provided": burned_fee.to_string(),
                "expected": expected_fee.to_string(),
                "taxable_amount": taxable_amount.to_string()
            })),
        );
    }

    // ============================================================
    // 3) Chiffrement — destinataires = base client + toutes les adresses de sortie
    // ============================================================
    // Le frais étant brûlé (aucun output de frais), il n'y a plus de clé
    // coordinateur à ajouter. On ajoute la clé X25519 de CHAQUE adresse de sortie
    // (destinataire, change, bénéficiaire de transfer-fee) pour qu'ils puissent
    // déchiffrer leur UTXO.
    let mut recipients_xpk = body.recipients_xpk.clone();
    for out in &tx.outputs {
        if let Ok((_h20, xpk)) = pms_wallet::decode_address(&out.address) {
            if !recipients_xpk.contains(&xpk) {
                recipients_xpk.push(xpk);
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
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": e })));
        }
    };

    // ============================================================
    // 5) Forge bloc + WireBlock + signature
    // ============================================================
    // `adapter` déjà lié plus haut (validation) — on le réutilise.
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
    // Sender address for activity indexing — réutilise la valeur déjà résolue
    // par `validate_txutxo_full` (premier input). Évite un second `get_utxo`
    // qui, après l'application du delta, renverrait `None` (input consommé) et
    // raterait l'indexation de l'émetteur.
    let sender_addr = sender_address.clone();

    // For encrypted TxUtxo payloads we hand the plaintext delta to the
    // adapter so it's applied in the same critical section as the block
    // insert (audit finding H1). For non-TxUtxo encrypted payloads
    // (e.g. `LedgerOwnershipTransfer`) there's no UTXO delta to apply,
    // so we fall back to the delta-less path.
    let persist_result = if let PlainPayload::TxUtxo(ref tx) = plain {
        tx_helpers::persist_and_broadcast_with_delta(&state, &wb, &tx.inputs, &tx.outputs).await
    } else {
        tx_helpers::persist_and_broadcast(&state, &wb).await
    };

    match persist_result {
        Ok(PutResult::Inserted) => {

            // Index activity for encrypted payload (both untyped + typed).
            // Plain payloads are indexed automatically in append_block_atomic_with_utxo,
            // but encrypted payloads need explicit indexing since the coordinator
            // knows the plain payload before encryption.
            {
                let mut addrs = pms_storage::helpers::extract_involved_addresses(&plain);
                let mut typed = pms_storage::helpers::extract_involved_with_category(&plain);
                // Add sender to indexed addresses (extract_involved_addresses only
                // returns output addresses for TxUtxo, but the sender may have no
                // change output and would be missed).
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

            // Accumulate the ACTUAL burned fee (`in − out`), NOT the client-
            // declared `tx.fee`. The distributor re-mints the pool to nodes, so
            // crediting more than was burned would over-mint (inflation) if a
            // client over-declares `tx.fee` above the real burn. Crediting the
            // real burn keeps `Σ minted == Σ burned` → net-zero (audit rang 2c).
            tx_helpers::accumulate_tx_fee(&state, burned_fee).await;

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
    /// Frais de gas PMS calculés (string décimale)
    pub fee: String,
    /// Frais de transfert smart contract (string décimale, dans le même asset que le transfert).
    /// `"0"` si aucun contrat de transfert n'est actif sur ce ledger.
    pub transfer_fee: String,
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

    // Fail-fast : destinataire valide (sinon output construit vers un piège).
    if let Some(rejection) = crate::api_fn::recipient::reject_bad_recipient(&req.to) {
        return rejection;
    }

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

    // ════════════════════════════════════════════════════════════════════════
    // 2.a) Évaluer les contrats de frais de transfert (smart contract fees)
    // ════════════════════════════════════════════════════════════════════════
    let transfer_fees = evaluate_transfer(
        state.contract_store.as_ref(),
        &state.ledger_id,
        req.asset_id.as_deref(),
        amount_dec,
    );
    let total_transfer_fee: Decimal = transfer_fees.iter().map(|f| f.fee_amount).sum();

    // For PMS native: total_needed = amount + fee + transfer_fee
    // For custom tokens: total_needed = amount + transfer_fee (PMS gas fee is separate)
    let total_needed = if req.asset_id.is_some() {
        amount_dec + total_transfer_fee
    } else {
        amount_dec + fee_dec + total_transfer_fee
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
            tx_inputs.push(TxInput {
                out: output_id.clone(),
            });
            inputs_detail.push(UtxoDetail {
                txid: output_id.txid.clone(),
                index: output_id.index,
                amount: amt.to_string(),
            });
        }
        pms_change = pms_sum - fee_dec;
    }

    // ════════════════════════════════════════════════════════════════════════
    // 6) Construire les outputs (modèle frais-brûlé-à-la-source, phase 2b)
    //    - Output 1: destination (to, amount, asset_id)
    //    - Outputs transfer-fee (smart contract) — même asset que le transfert
    //    - Output: change vers sender (from, change, asset_id) [si > 0]
    //    - Output: PMS change vers sender [si custom token + PMS change > 0]
    //    PAS d'output de frais de gas : le frais (`fee_dec`) est BRÛLÉ à la
    //    source (`Σ inputs PMS − Σ outputs PMS`), déjà soustrait du `change`
    //    ci-dessous. Le validateur (conservation `out ≤ in`) et `wallet_send_tx`
    //    (frais implicite ≥ minimum) l'enforcent.
    // ════════════════════════════════════════════════════════════════════════
    let mut tx_outputs: Vec<TxOutput> = Vec::new();

    // Output destination
    tx_outputs.push(TxOutput::new(req.to.clone(), amount_dec.to_string(), req.asset_id.clone()));

    // Outputs frais de transfert (smart contract) — même asset que le transfert
    for fee_result in &transfer_fees {
        tx_outputs.push(TxOutput::new(fee_result.beneficiary_address.clone(), fee_result.fee_amount.to_string(), req.asset_id.clone()));
    }

    // Change (retour vers l'expéditeur) — same asset as the transfer.
    // `total_needed` inclut déjà `fee_dec` (pour le PMS natif), donc le change
    // est minoré du frais → `in − out = fee` (brûlé).
    let change = selected_sum - total_needed;
    if change > Decimal::ZERO {
        tx_outputs.push(TxOutput::new(req.from.clone(), change.to_string(), req.asset_id.clone()));
    }

    // PMS change (only for custom token transfers where we also spent PMS for fees).
    // `pms_change = pms_sum − fee_dec` : le frais PMS est brûlé (pas d'output).
    if pms_change > Decimal::ZERO {
        tx_outputs.push(TxOutput::new(req.from.clone(), pms_change.to_string(), None,));
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
    let tx_hash = match unsigned_tx.signing_message(&state.settings.network.network_id) {
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
            transfer_fee: total_transfer_fee.to_string(),
            inputs_detail,
        })),
    )
}
