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
use rust_decimal::prelude::FromPrimitive;

use super::distribute::DistributeFeesResult;

/// Executes a daily inflation mint based on the circulating supply.
/// daily_amount = circulating_supply * annual_inflation_percent / 365
/// Distributed according to creator_reward_percent / treasury_reward_percent / burn_percent.
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
        return Ok(DistributeFeesResult {
            success: false,
            reward_block_id: None,
            total_distributed: "0".to_string(),
            num_recipients: 0,
        });
    }

    // 1. GET CIRCULATING SUPPLY
    let (circulating_supply, _) = state.srv.adapter_arc().circulating_supply().await;
    if circulating_supply <= Decimal::ZERO {
        tracing::info!("📊 Inflation mint skipped: circulating supply is 0");
        return Ok(DistributeFeesResult {
            success: true,
            reward_block_id: None,
            total_distributed: "0".to_string(),
            num_recipients: 0,
        });
    }

    // 2. CALCULATE DAILY AMOUNT
    let annual_rate = Decimal::from_f64(settings.fees.annual_inflation_percent)
        .unwrap_or(Decimal::ZERO)
        / Decimal::from(100);
    let daily_amount = (circulating_supply * annual_rate / Decimal::from(365)).round_dp(8);

    if daily_amount <= Decimal::ZERO {
        tracing::info!("📊 Inflation mint skipped: daily amount rounds to 0");
        return Ok(DistributeFeesResult {
            success: true,
            reward_block_id: None,
            total_distributed: "0".to_string(),
            num_recipients: 0,
        });
    }

    tracing::info!(
        "📊 Inflation mint: supply={}, rate={}%/year, daily={}",
        circulating_supply,
        settings.fees.annual_inflation_percent,
        daily_amount
    );

    // 3. COMPUTE DISTRIBUTION
    let creator_pct = Decimal::from(settings.fees.creator_reward_percent);
    let treasury_pct = Decimal::from(settings.fees.treasury_reward_percent);
    // burn_percent is implicit (not minted)

    let coordinator_amount = (daily_amount * creator_pct / Decimal::from(100)).round_dp(8);
    let treasury_amount = (daily_amount * treasury_pct / Decimal::from(100)).round_dp(8);

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
        return Ok(DistributeFeesResult {
            success: true,
            reward_block_id: None,
            total_distributed: "0".to_string(),
            num_recipients: 0,
        });
    }

    let num_recipients = all_outputs.len();
    let burned = daily_amount - total_distributed;

    tracing::info!(
        "📊 Inflation distribution: {} coordinator, {} treasury, {} burned",
        coordinator_amount,
        treasury_amount,
        burned
    );

    // 4. RESOLVE PARENT
    let parent_id = match state.srv.adapter_arc().top_tips(1).await {
        Ok(tips) if !tips.is_empty() => tips[0].clone(),
        _ => {
            return Ok(DistributeFeesResult {
                success: false,
                reward_block_id: None,
                total_distributed: "0".to_string(),
                num_recipients: 0,
            });
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
                "Daily inflation: {} PMS ({}%/year, {} burned)",
                total_distributed, settings.fees.annual_inflation_percent, burned
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
            anyhow::bail!("Inflation block already exists")
        }
        Ok(PutResult::Rejected(r)) => {
            anyhow::bail!("Inflation block rejected: {}", r)
        }
        Err(e) => anyhow::bail!("Storage error: {}", e),
    }
}
