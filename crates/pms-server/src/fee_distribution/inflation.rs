use crate::api::AppState;
use anyhow::Result;
use pms_storage::PutResult;
use pms_types::TxOutput;
use pms_types_block::Block;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_utils::check_pow::check_pow_leading_zero_bits;
use pms_utils::compute_block_id;
use pms_wallet::SignerBackend;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::WireBlock;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use super::distribute::DistributeFeesResult;

/// Résultat « rien distribué » (mint sauté ou échoué). Factorise les retours
/// vides du baseline mint (5 chemins : non-coordinateur, persist du compteur
/// échoué, budget consommé, split vide, parent introuvable).
fn empty_result(success: bool) -> DistributeFeesResult {
    DistributeFeesResult {
        success,
        reward_block_id: None,
        total_distributed: "0".to_string(),
        num_recipients: 0,
    }
}

/// Exécute le baseline mint d'émission : minte le **résidu** du budget de la
/// période (`budget − déjà-émis-par-les-voies`), via le gate d'émission
/// (plan §3.1). Le budget est `supply × clamp(taux_cible, plancher, plafond) ×
/// frac_année` — le `clamp` au plafond est le couloir inviolable (plan §2.1).
/// Le montant minté est réparti `creator:treasury` (renormalisé, sans burn — la
/// cible EST le taux de croissance net).
pub async fn perform_daily_inflation_mint(state: &AppState) -> Result<DistributeFeesResult> {
    let settings = &state.settings;
    let node_wallet = &state.node_wallet;

    // Only Coordinator can mint
    let is_coordinator = if let Some(coord_pk) = &settings.validation.coordinator_public_key {
        node_wallet.encoded_public_key() == *coord_pk
    } else {
        true
    };

    if !is_coordinator {
        return Ok(empty_result(false));
    }

    // 1. RESERVE THE EMISSION BUDGET RESIDUAL (plan §3.1)
    //
    // The baseline tops up to the period target: it mints the *residual*
    // (`budget − already-emitted-by-all-voies`), bounded by the corridor.
    // The gate reads the current circulating supply, rolls/computes the epoch
    // budget (supply frozen at epoch start), and atomically reserves under its
    // mutex — closing the mint TOCTOU and persisting the counter BEFORE we forge
    // (counter-first crash safety). A reservation of 0 means the budget is
    // already consumed this epoch (or is zero): no-op, do NOT forge an empty
    // block. The corridor `clamp` caps the budget even if the configured target
    // rate is above the ceiling — that is the inviolable rule of plan §2.1.
    let (circulating_supply, _) = state.srv.adapter_arc().circulating_supply().await;
    let params = crate::emission::EmissionParams {
        target_pct: settings.fees.annual_inflation_percent,
        ceiling_pct: settings.fees.annual_ceiling_percent,
        floor_pct: settings.fees.annual_floor_percent,
        epoch_duration_sec: settings.fees.emission_epoch_duration_sec,
    };
    let reservation = match state
        .emission_gate
        .reserve(
            &state.store,
            pms_utils::ts_ms(),
            circulating_supply,
            params,
            crate::emission::Voie::Baseline,
            None,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            // Counter persist failed → nothing reserved. Skip this round.
            tracing::warn!("📊 Inflation mint skipped: {}", e);
            return Ok(empty_result(false));
        }
    };
    // Publish budget gauges from the post-reserve snapshot (refreshed every
    // tick, even when nothing is minted, so the dashboard stays current). The
    // effective-rate gauge is the corridor canary (alert if it ever exceeds the
    // ceiling — would mean the clamp failed).
    {
        let snap = state.emission_gate.snapshot().await;
        let eff = crate::emission::effective_rate_pct(
            settings.fees.annual_inflation_percent,
            settings.fees.annual_ceiling_percent,
            settings.fees.annual_floor_percent,
        );
        let lid = &state.ledger_id;
        crate::metrics::EMISSION_BUDGET_TOTAL
            .with_label_values(&[lid])
            .set(snap.budget.to_f64().unwrap_or(0.0));
        crate::metrics::EMISSION_BUDGET_CONSUMED
            .with_label_values(&[lid])
            .set(snap.emitted.to_f64().unwrap_or(0.0));
        crate::metrics::EMISSION_BUDGET_REMAINING
            .with_label_values(&[lid])
            .set(snap.remaining().to_f64().unwrap_or(0.0));
        crate::metrics::EMISSION_EFFECTIVE_RATE
            .with_label_values(&[lid])
            .set(eff);
    }

    let daily_amount = reservation.amount;
    if daily_amount <= Decimal::ZERO {
        tracing::info!(
            "📊 Inflation mint skipped: budget already consumed this epoch (supply={})",
            circulating_supply
        );
        return Ok(empty_result(true));
    }

    tracing::info!(
        "📊 Inflation mint (residual): supply={}, target={}%/yr, ceiling={}%/yr, epoch={}s, amount={}",
        circulating_supply,
        settings.fees.annual_inflation_percent,
        settings.fees.annual_ceiling_percent,
        settings.fees.emission_epoch_duration_sec,
        daily_amount
    );

    // 2. SPLIT (renormalised creator:treasury). The FULL reserved amount is
    // minted — the corridor target IS the net supply-growth target, so no
    // separate inflation burn is applied here (the deflationary rake burn lives
    // in the fee path, `burn_rate_bps`). Reserving == minting keeps the budget
    // counter exact (no reserve/mint mismatch).
    let creator_pct = Decimal::from(settings.fees.creator_reward_percent);
    let treasury_pct = Decimal::from(settings.fees.treasury_reward_percent);
    let denom = creator_pct + treasury_pct;
    let coordinator_amount = if denom > Decimal::ZERO {
        (daily_amount * creator_pct / denom).round_dp(8)
    } else {
        daily_amount
    };
    let treasury_amount = daily_amount - coordinator_amount; // exact remainder, no dust

    let coordinator_address = node_wallet.get_address("8e");
    let treasury_addr = state
        .treasury_wallets
        .list
        .first()
        .cloned()
        .or_else(|| settings.fees.treasury_addresses.first().cloned())
        .unwrap_or_else(|| coordinator_address.clone());

    let mut all_outputs: Vec<TxOutput> = Vec::new();
    let mut total_distributed = Decimal::ZERO;

    if coordinator_amount > Decimal::ZERO {
        all_outputs.push(TxOutput::new(coordinator_address.clone(), coordinator_amount.normalize().to_string(), None));
        total_distributed += coordinator_amount;
    }

    if treasury_amount > Decimal::ZERO {
        all_outputs.push(TxOutput::new(treasury_addr.clone(), treasury_amount.normalize().to_string(), None));
        total_distributed += treasury_amount;
    }

    if all_outputs.is_empty() {
        // Nothing to mint after the split — release the reservation so the
        // budget isn't leaked.
        state.emission_gate.release(&state.store, daily_amount).await;
        return Ok(empty_result(true));
    }

    let num_recipients = all_outputs.len();

    tracing::info!(
        "📊 Inflation distribution: {} coordinator, {} treasury",
        coordinator_amount,
        treasury_amount
    );

    // 4. RESOLVE PARENT — on failure, release the reservation (no block forged).
    let parent_id = match state.srv.adapter_arc().top_tips(1).await {
        Ok(tips) if !tips.is_empty() => tips[0].clone(),
        _ => {
            state.emission_gate.release(&state.store, daily_amount).await;
            return Ok(empty_result(false));
        }
    };

    // 5. CREATE MINT BLOCK
    let mint_payload = PlainPayload::Mint {
        outputs: all_outputs.clone(),
    };

    let coordinator_x25519 = node_wallet.x25519_pub_hex().to_string();
    let mut block = Block {
        id: String::new(),
        parents: vec![parent_id],
        payload: Some(PayloadEnvelope::Plain(mint_payload)),
        nonce: 0,
        metadata: Some(pms_types_block::BlockMetadata {
            signer_x25519_hex: Some(coordinator_x25519),
            description: Some(format!(
                "Daily inflation (residual): {} PMS (target {}%/yr, ceiling {}%/yr)",
                total_distributed,
                settings.fees.annual_inflation_percent,
                settings.fees.annual_ceiling_percent
            )),
            ..Default::default()
        }),
        signer_pk: None,
        signature: None,
    };
    block.id = compute_block_id(&block.parents, &block.payload, block.nonce);

    // PoW
    let min_bits = state.srv.adapter_arc().min_pow_leading_zero_bits();
    if min_bits > 0 {
        while !check_pow_leading_zero_bits(&block.id, min_bits) {
            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
        }
    }

    // 6. BUILD WIREBLOCK & SIGN
    let payload_json = serde_json::to_string(&block.payload)?;
    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json: Some(payload_json),
        nonce: block.nonce,
        network_id: state.settings.network.network_id.clone(),
        protocol_version: state.settings.network.protocol_version as u16,
        signer_pk_hex: node_wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: block.metadata.clone(),
    };

    let msg = canonical_wireblock_message(&wb);
    wb.signature_hex = node_wallet.sign(&msg)?;

    // 7. PERSIST & UPDATE UTXOs
    match state.srv.adapter_arc().persist_block(&wb).await {
        Ok(PutResult::Inserted) => {
            crate::metrics::BLOCKS_PERSISTED
                .with_label_values(&[&state.ledger_id])
                .inc();
            crate::metrics::EMISSION_MINTED
                .with_label_values(&[
                    state.ledger_id.as_str(),
                    crate::emission::Voie::Baseline.as_str(),
                ])
                .inc_by(total_distributed.to_f64().unwrap_or(0.0));
            let _ = state.srv.enqueue_broadcast(wb.id.clone()).await;

            // NOTE: No add_utxo here — PlainPayload::Mint is a plain payload,
            // so persist_block() already constructs the UtxoDelta and applies it
            // via apply_diff(). Calling add_utxo again would double-count supply
            // AND destroy the address index (LRU re-insert evicts existing entry).

            tracing::info!(
                "📊 Daily inflation minted: {} PMS to {} wallets (block: {})",
                total_distributed,
                num_recipients,
                &wb.id[..16]
            );

            Ok(DistributeFeesResult {
                success: true,
                reward_block_id: Some(wb.id),
                total_distributed: total_distributed.to_string(),
                num_recipients,
            })
        }
        Ok(PutResult::AlreadyExists) => {
            // The block (and its supply) already exist — our reservation was a
            // duplicate. Release it so the budget isn't double-charged.
            state.emission_gate.release(&state.store, daily_amount).await;
            anyhow::bail!("Inflation block already exists")
        }
        Ok(PutResult::Rejected(r)) => {
            state.emission_gate.release(&state.store, daily_amount).await;
            anyhow::bail!("Inflation block rejected: {}", r)
        }
        Err(e) => {
            state.emission_gate.release(&state.store, daily_amount).await;
            anyhow::bail!("Storage error: {}", e)
        }
    }
}
