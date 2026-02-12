use crate::{
    api::AppState,
    fee_distribution::{FeeDistributionConfig, compute_fee_outputs},
};
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use http::StatusCode;
use pms_config::RuntimeConfig;
use pms_storage::{ConfigStorage, DagStorage, PutResult};
use pms_token::FeePolicy;
use pms_types::{Block, OutputId, Transaction, TxInput, TxOutput};
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_utils::{check_pow_leading_zero_bits, compute_block_id};
use pms_wallet::SignerBackend;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::{WireBlock, WireMeta};
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
    let meta = WireMeta::from(settings);

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
    let runtime_config = state
        .store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());

    let ratio_dec = Decimal::from(runtime_config.fee_rate_bps) / Decimal::from(10000);

    let fee_policy = FeePolicy::new(
        &runtime_config.base_fee,
        &ratio_dec.to_string(), // Convert config bps to ratio string (ex: "0.01")
    );

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
    // 4) Parents via store - use genesis as supplementary parent if needed
    // ============================================================
    let mut parents = match state.store.top_tips(2).await {
        Ok(tips) if !tips.is_empty() => tips,
        _ => match state.store.all_block_ids().await {
            Ok(ids) if !ids.is_empty() => vec![ids[0].clone()],
            _ => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({ "error": "no parents available (empty DAG)" })),
                );
            }
        },
    };

    // Single Writer Mode: exactly 1 parent required
    // Regular DAG mode: 2 parents required (add genesis if needed)
    if settings.validation.enforce_single_writer {
        // Keep only the first parent
        parents.truncate(1);
    } else {
        // If we have fewer than 2 parents and genesis isn't already included,
        // add genesis as supplementary parent
        if parents.len() < 2 {
            let genesis_id = Block::genesis(compute_block_id).id;
            if !parents.contains(&genesis_id) {
                parents.push(genesis_id);
            }
        }
    }

    // ============================================================
    // 5) Forge bloc + WireBlock + signature + persist
    // ============================================================
    // Include signer's X25519 public key in metadata for fee distribution
    // This allows parent block creators to receive their share of fees
    let block_metadata = pms_types_block::BlockMetadata {
        signer_x25519_hex: Some(state.node_wallet.x25519_pub_hex().to_string()),
        ..Default::default()
    };

    let mut block = Block {
        id: String::new(),
        parents: parents.clone(),
        payload: payload.clone(),
        nonce: 0,
        metadata: Some(block_metadata),
        signer_pk: None,
        signature: None,
    };
    block.id = compute_block_id(&block.parents, &block.payload, block.nonce);

    // PoW Mining Loop
    // Use the policy from the active adapter to ensure we meet the actual validation requirements
    let min_bits = state.srv.adapter_arc().min_pow_leading_zero_bits();

    if min_bits > 0 {
        loop {
            // Check difficulty using shared util
            if check_pow_leading_zero_bits(&block.id, min_bits) {
                break;
            }

            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);

            // Safety break
            if block.nonce == u64::MAX {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "mining failed (nonce exhaustion)" })),
                );
            }
        }
    }

    let payload_json = match &block.payload {
        None => None,
        Some(env) => match serde_json::to_string(env) {
            Ok(s) => Some(s),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("payload serialize error: {e:#}") })),
                );
            }
        },
    };

    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json,
        nonce: block.nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: block.metadata.clone(),
    };

    let node_wallet = &state.node_wallet;
    wb.signer_pk_hex = node_wallet.encoded_public_key();

    let msg = canonical_wireblock_message(&wb);
    wb.signature_hex = match node_wallet.sign(&msg) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("sign error: {e:?}") })),
            );
        }
    };

    let adapter = state.srv.adapter_arc();
    let res = match adapter.persist_block(&wb).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("persist error: {e:#}") })),
            );
        }
    };

    match res {
        PutResult::Inserted => {
            // ============================================================
            // FIX: Apply UTXO delta manually for encrypted payloads
            // persist_block only handles Plain payloads, so we need to
            // update the UTXO cache ourselves for encrypted transactions.
            // ============================================================
            // Extract the tx from the plain payload (tx was moved into it at line 244)
            if let PlainPayload::TxUtxo(ref tx) = plain {
                // Spend inputs (remove from UTXO cache)
                for input in &tx.inputs {
                    let output_id = pms_types::OutputId {
                        txid: input.out.txid.clone(),
                        index: input.out.index,
                    };
                    adapter.remove_utxo(&output_id).await;
                }
                // Create outputs (add to UTXO cache)
                for (idx, output) in tx.outputs.iter().enumerate() {
                    adapter
                        .add_utxo(
                            wb.id.clone(),
                            idx as u32,
                            output.address.clone(),
                            output.amount.clone(),
                            output.asset_id.clone(),
                        )
                        .await;
                }
                tracing::info!(
                    "📦 UTXO delta applied for encrypted TX: -{} inputs, +{} outputs",
                    tx.inputs.len(),
                    tx.outputs.len()
                );
            }

            // Announce the new block to the network for gossip propagation
            state.srv.enqueue_broadcast(wb.id.clone()).await;

            // ============================================================
            // 6) CREATE REWARD BLOCK (Fee Distribution + Block Rewards)
            // ============================================================
            // SECURITY: Only Coordinator creates Reward blocks
            // This prevents unauthorized token creation

            // Check if this node is the Coordinator
            let is_coordinator = if let Some(coord_pk) = &settings.validation.coordinator_public_key
            {
                // Compare node's public key with configured coordinator key
                node_wallet.encoded_public_key() == *coord_pk
            } else {
                // Dev mode: no coordinator check
                true
            };

            if is_coordinator {
                // Crée un bloc de reward séparé qui distribue:
                // - 65% Coordinator, 35% Treasury (des tx fees)
                // - 70% Creator, 20% Treasury, 10% Burn (block rewards)

                let fee_config = FeeDistributionConfig::new(
                    settings.fees.coordinator_fee_percent,
                    settings.fees.treasury_fee_percent,
                );

                let coordinator_address = node_wallet.get_address("8e");

                // Get treasury addresses: prefer signed list from AppState, fallback to config
                let treasury_addrs: Vec<String> = if !state.treasury_wallets.is_empty() {
                    tracing::debug!(
                        "Using {} treasury wallets from signed list",
                        state.treasury_wallets.len()
                    );
                    state.treasury_wallets.list.clone()
                } else if !settings.fees.treasury_addresses.is_empty() {
                    tracing::debug!(
                        "Using {} treasury addresses from config",
                        settings.fees.treasury_addresses.len()
                    );
                    settings.fees.treasury_addresses.clone()
                } else {
                    tracing::debug!(
                        "Treasury empty, using {} admin addresses as fallback",
                        settings.admin.wallet_addresses.len()
                    );
                    settings.admin.wallet_addresses.clone()
                };

                // Calcul des fee outputs (65% coordinator, 35% treasury)
                let fee_outputs_raw = compute_fee_outputs(
                    fee_dec,
                    &treasury_addrs,
                    &coordinator_address,
                    &fee_config,
                );

                // Log fee distribution details for debugging
                tracing::info!(
                    "Fee distribution: {} fee outputs, {} treasury addrs",
                    fee_outputs_raw.len(),
                    treasury_addrs.len()
                );
                for fo in &fee_outputs_raw {
                    tracing::info!(
                        "  Fee output: {} -> {}",
                        &fo.address[..20.min(fo.address.len())],
                        fo.amount
                    );
                }

                // Si on a des fee outputs à distribuer, créer un reward block
                if !fee_outputs_raw.is_empty() {
                    // Convertir en TxOutput
                    let fee_txouts: Vec<TxOutput> = fee_outputs_raw
                        .iter()
                        .map(|fo| TxOutput {
                            address: fo.address.clone(),
                            amount: fo.amount.clone(),
                            asset_id: None, // fees always PMS
                        })
                        .collect();

                    // Payload Reward (fees only, no block rewards)
                    let reward_payload = PlainPayload::Reward {
                        fee_outputs: fee_txouts,
                        reward_outputs: vec![],
                        burned: "0".to_string(),
                        tx_block_id: wb.id.clone(),
                    };

                    // Créer le bloc de reward (parent = le bloc TX qu'on vient de créer)
                    let mut reward_block = Block {
                        id: String::new(),
                        parents: vec![wb.id.clone()],
                        payload: Some(PayloadEnvelope::Plain(reward_payload)),
                        nonce: 0,
                        metadata: Some(pms_types_block::BlockMetadata {
                            signer_x25519_hex: Some(state.node_wallet.x25519_pub_hex().to_string()),
                            description: Some("Reward distribution".to_string()),
                            ..Default::default()
                        }),
                        signer_pk: None,
                        signature: None,
                    };
                    reward_block.id = compute_block_id(
                        &reward_block.parents,
                        &reward_block.payload,
                        reward_block.nonce,
                    );

                    // PoW (minimal pour reward blocks)
                    let min_bits = state.srv.adapter_arc().min_pow_leading_zero_bits();
                    if min_bits > 0 {
                        while !check_pow_leading_zero_bits(&reward_block.id, min_bits) {
                            reward_block.nonce += 1;
                            reward_block.id = compute_block_id(
                                &reward_block.parents,
                                &reward_block.payload,
                                reward_block.nonce,
                            );
                        }
                    }

                    // Build WireBlock pour le reward
                    let reward_payload_json = serde_json::to_string(&reward_block.payload).ok();
                    let mut reward_wb = WireBlock {
                        id: reward_block.id.clone(),
                        parents: reward_block.parents.clone(),
                        payload_json: reward_payload_json,
                        nonce: reward_block.nonce,
                        network_id: meta.network_id.clone(),
                        protocol_version: meta.protocol_version as u16,
                        signer_pk_hex: node_wallet.encoded_public_key(),
                        signature_hex: String::new(),
                        metadata: reward_block.metadata.clone(),
                    };

                    // Signer
                    let reward_msg = canonical_wireblock_message(&reward_wb);
                    if let Ok(sig) = node_wallet.sign(&reward_msg) {
                        reward_wb.signature_hex = sig;

                        // Persister
                        if let Ok(PutResult::Inserted) = adapter.persist_block(&reward_wb).await {
                            state.srv.enqueue_broadcast(reward_wb.id.clone()).await;
                            tracing::info!(
                                "📦 Reward block created: {} (fees distributed from TX {})",
                                &reward_wb.id[..16],
                                &wb.id[..16]
                            );
                        }
                    }
                }
            } // End of if is_coordinator

            (
                StatusCode::CREATED,
                Json(json!({ "id": wb.id, "status": "inserted" })),
            )
        }
        PutResult::AlreadyExists => (
            StatusCode::CONFLICT,
            Json(json!({ "id": wb.id, "status": "duplicate" })),
        ),
        PutResult::Rejected(reason) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "status": "rejected", "reason": reason })),
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
    let runtime_config = state
        .store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());

    let ratio_dec = Decimal::from(runtime_config.fee_rate_bps) / Decimal::from(10000);
    let fee_policy = FeePolicy::new(&runtime_config.base_fee, &ratio_dec.to_string());

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
    // 3) Récupérer les UTXOs de l'expéditeur (via adapter RAM)
    //    Filtrés par asset_id (None = PMS natif uniquement)
    // ════════════════════════════════════════════════════════════════════════
    let adapter = state.srv.adapter_arc();
    let all_utxos = adapter.utxos_by_address(&req.from).await;

    // Filter by asset_id: only select UTXOs matching the requested asset
    let utxos: Vec<_> = all_utxos
        .into_iter()
        .filter(|(_, tx_output)| tx_output.asset_id == req.asset_id)
        .collect();

    if utxos.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "no UTXOs found for sender address",
                "address": req.from,
                "asset_id": req.asset_id
            })),
        );
    }

    // For custom tokens, also check PMS UTXOs for fee coverage
    let pms_utxos: Vec<_> = if req.asset_id.is_some() && fee_dec > Decimal::ZERO {
        adapter
            .utxos_by_address(&req.from)
            .await
            .into_iter()
            .filter(|(_, tx_output)| tx_output.asset_id.is_none())
            .collect()
    } else {
        Vec::new()
    };

    // ════════════════════════════════════════════════════════════════════════
    // 4) Coin Selection (algorithme simple: largest-first)
    //    On sélectionne les plus gros UTXOs jusqu'à couvrir total_needed
    // ════════════════════════════════════════════════════════════════════════
    let mut utxo_list: Vec<_> = utxos
        .into_iter()
        .filter_map(|(output_id, tx_output)| {
            Decimal::from_str_exact(&tx_output.amount)
                .ok()
                .map(|amt| (output_id, tx_output, amt))
        })
        .collect();

    // Trier par montant décroissant (largest-first)
    utxo_list.sort_by(|a, b| b.2.cmp(&a.2));

    let mut selected_inputs: Vec<(OutputId, TxOutput, Decimal)> = Vec::new();
    let mut selected_sum = Decimal::ZERO;

    for (output_id, tx_output, amt) in utxo_list {
        if selected_sum >= total_needed {
            break;
        }
        selected_sum += amt;
        selected_inputs.push((output_id, tx_output, amt));
    }

    // Vérifier qu'on a assez de fonds
    if selected_sum < total_needed {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "insufficient balance",
                "available": selected_sum.to_string(),
                "required": total_needed.to_string(),
                "amount": amount_dec.to_string(),
                "fee": fee_dec.to_string()
            })),
        );
    }

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
        let mut pms_list: Vec<_> = pms_utxos
            .into_iter()
            .filter_map(|(output_id, tx_output)| {
                Decimal::from_str_exact(&tx_output.amount)
                    .ok()
                    .map(|amt| (output_id, tx_output, amt))
            })
            .collect();
        pms_list.sort_by(|a, b| b.2.cmp(&a.2));

        let mut pms_selected_sum = Decimal::ZERO;
        for (output_id, _, amt) in &pms_list {
            if pms_selected_sum >= fee_dec {
                break;
            }
            pms_selected_sum += *amt;
            tx_inputs.push(TxInput { out: output_id.clone() });
            inputs_detail.push(UtxoDetail {
                txid: output_id.txid.clone(),
                index: output_id.index,
                amount: amt.to_string(),
            });
        }

        if pms_selected_sum < fee_dec {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": "insufficient PMS balance for fee",
                    "pms_available": pms_selected_sum.to_string(),
                    "fee_required": fee_dec.to_string()
                })),
            );
        }
        pms_change = pms_selected_sum - fee_dec;
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
