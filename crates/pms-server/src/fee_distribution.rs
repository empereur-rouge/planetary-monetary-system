// ═══════════════════════════════════════════════════════════════════════════════
// Fee Distribution Module
// ═══════════════════════════════════════════════════════════════════════════════
//
// Ce module gère la distribution des frais de transaction entre :
// - Le Coordinator : 65% par défaut
// - Le Treasury (wallets admin) : 35% par défaut
//
// Mode centralisé : seul le Coordinator traite les transactions.
// ═══════════════════════════════════════════════════════════════════════════════

use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
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
use serde::{Deserialize, Serialize};

/// Représente un output de fee à inclure dans le bloc
#[derive(Debug, Clone)]
pub struct FeeOutput {
    pub address: String,
    pub amount: String,
}

/// Configuration pour la distribution des fees (Coordinator + Treasury)
#[derive(Debug, Clone)]
pub struct FeeDistributionConfig {
    /// Pourcentage vers le coordinator
    pub coordinator_percent: u8,
    /// Pourcentage vers le treasury (admin wallets)
    pub treasury_percent: u8,
}

impl Default for FeeDistributionConfig {
    fn default() -> Self {
        Self {
            coordinator_percent: 65,
            treasury_percent: 35,
        }
    }
}

impl FeeDistributionConfig {
    pub fn new(coordinator: u8, treasury: u8) -> Self {
        Self {
            coordinator_percent: coordinator,
            treasury_percent: treasury,
        }
    }

    /// Valide que les pourcentages totalisent 100%
    pub fn validate(&self) -> Result<(), String> {
        let total = self.coordinator_percent + self.treasury_percent;
        if total != 100 {
            return Err(format!("Fee percentages must sum to 100, got {}", total));
        }
        Ok(())
    }
}

/// Calcule les outputs de fee pour une transaction (Coordinator + Treasury)
///
/// # Arguments
/// * `total_fee` - Le montant total des fees
/// * `treasury_addresses` - Liste des adresses treasury (choisie pseudo-aléatoirement)
/// * `coordinator_address` - Adresse du coordinator
/// * `config` - Configuration de distribution
///
/// # Returns
/// Liste des FeeOutput à inclure dans le payload
pub fn compute_fee_outputs(
    total_fee: Decimal,
    treasury_addresses: &[String],
    coordinator_address: &str,
    config: &FeeDistributionConfig,
) -> Vec<FeeOutput> {
    let mut outputs = Vec::new();

    if let Err(e) = config.validate() {
        eprintln!("[FEE] Config validation error: {}, using defaults", e);
    }

    let coordinator_amount =
        total_fee * Decimal::from_u8(config.coordinator_percent).unwrap() / Decimal::from(100);
    let treasury_amount =
        total_fee * Decimal::from_u8(config.treasury_percent).unwrap() / Decimal::from(100);

    // 1) Coordinator
    if coordinator_amount > Decimal::ZERO {
        outputs.push(FeeOutput {
            address: coordinator_address.to_string(),
            amount: coordinator_amount.normalize().to_string(),
        });
    }

    // 2) Treasury
    if treasury_amount > Decimal::ZERO {
        if !treasury_addresses.is_empty() {
            use rand::Rng;
            let idx = rand::rng().random_range(0..treasury_addresses.len());
            outputs.push(FeeOutput {
                address: treasury_addresses[idx].clone(),
                amount: treasury_amount.normalize().to_string(),
            });
        } else {
            // Fallback: pas de treasury wallet -> tout au coordinator
            eprintln!("[FEE] Warning: No treasury wallet configured. Fallback to coordinator.");
            outputs.push(FeeOutput {
                address: coordinator_address.to_string(),
                amount: treasury_amount.normalize().to_string(),
            });
        }
    }

    outputs
}

/// Configuration pour les récompenses de bloc (inflation)
#[derive(Debug, Clone)]
pub struct BlockRewardConfig {
    /// Récompense par bloc (ex: "0.1")
    pub reward_per_block: String,
    /// Pourcentage vers le créateur
    pub creator_percent: u8,
    /// Pourcentage vers le treasury
    pub treasury_percent: u8,
    /// Pourcentage à brûler
    pub burn_percent: u8,
}

impl Default for BlockRewardConfig {
    fn default() -> Self {
        Self {
            reward_per_block: "0.1".to_string(),
            creator_percent: 70,
            treasury_percent: 20,
            burn_percent: 10,
        }
    }
}

/// Calcule les outputs de récompense pour un nouveau bloc
///
/// # Arguments
/// * `creator_address` - Adresse du créateur du bloc
/// * `treasury_address` - Adresse treasury pour recevoir la part
/// * `config` - Configuration des récompenses
///
/// # Returns
/// (Vec<FeeOutput>, burn_amount) - les outputs et le montant à brûler
pub fn compute_block_reward_outputs(
    creator_address: &str,
    treasury_address: &str,
    config: &BlockRewardConfig,
) -> (Vec<FeeOutput>, Decimal) {
    let mut outputs = Vec::new();

    let total: Decimal = config.reward_per_block.parse().unwrap_or(Decimal::ZERO);
    if total == Decimal::ZERO {
        return (outputs, Decimal::ZERO);
    }

    let creator_amount =
        total * Decimal::from_u8(config.creator_percent).unwrap() / Decimal::from(100);
    let treasury_amount =
        total * Decimal::from_u8(config.treasury_percent).unwrap() / Decimal::from(100);
    let burn_amount = total * Decimal::from_u8(config.burn_percent).unwrap() / Decimal::from(100);

    // Créateur
    if creator_amount > Decimal::ZERO {
        outputs.push(FeeOutput {
            address: creator_address.to_string(),
            amount: creator_amount.normalize().to_string(),
        });
    }

    // Treasury
    if treasury_amount > Decimal::ZERO {
        outputs.push(FeeOutput {
            address: treasury_address.to_string(),
            amount: treasury_amount.normalize().to_string(),
        });
    }

    (outputs, burn_amount)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Automated Fee Distribution Logic
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributeFeesResult {
    pub success: bool,
    pub reward_block_id: Option<String>,
    pub total_distributed: String,
    pub num_recipients: usize,
}

/// Exécute la distribution des fees et refunds accumulés dans le pool.
/// Crée un bloc de type Mint contenant les UTXOs pour les destinataires.
pub async fn perform_fee_distribution(
    state: &AppState,
    parent_id: Option<String>,
) -> Result<DistributeFeesResult> {
    let settings = &state.settings;
    let node_wallet = &state.node_wallet;

    // Only Coordinator can distribute
    let is_coordinator = if let Some(coord_pk) = &settings.validation.coordinator_public_key {
        node_wallet.encoded_public_key() == *coord_pk
    } else {
        true // Dev mode
    };

    if !is_coordinator {
        // Not authorized, but we return success=false instead of logging error essentially
        return Ok(DistributeFeesResult {
            success: false,
            reward_block_id: None,
            total_distributed: "0".to_string(),
            num_recipients: 0,
        });
    }

    // 0. RESOLVE PARENT
    let parent_id = match parent_id {
        Some(id) if !id.is_empty() => id,
        _ => {
            // Fetch current tip from DAG
            match state.srv.adapter_arc().top_tips(1).await {
                Ok(tips) if !tips.is_empty() => tips[0].clone(),
                _ => {
                    // No tips available? Should be rare unless genesis
                    return Ok(DistributeFeesResult {
                        success: false,
                        reward_block_id: None,
                        total_distributed: "0".to_string(),
                        num_recipients: 0,
                    });
                }
            }
        }
    };

    // 1. READ FEE POOL
    let (total_node_fees, shares, burn_refunds, total_burn_refunds) = {
        let pool = state.fee_pool.read().await;
        if !pool.has_fees() {
            return Ok(DistributeFeesResult {
                success: true, // "Success" because nothing to do
                reward_block_id: None,
                total_distributed: "0".to_string(),
                num_recipients: 0,
            });
        }
        (
            pool.total_fees,
            pool.calculate_shares(),
            pool.get_burn_refunds(),
            pool.total_burn_refunds(),
        )
    };

    // 2. BUILD OUTPUTS
    let coordinator_x25519 = state.node_wallet.x25519_pub_hex().to_string();
    let mut all_outputs: Vec<TxOutput> = Vec::new();
    let mut total_distributed = Decimal::ZERO;

    // 2a. BURN REFUNDS
    for (wallet_address, amount) in &burn_refunds {
        if *amount <= Decimal::ZERO {
            continue;
        }
        all_outputs.push(TxOutput {
            address: wallet_address.clone(),
            amount: amount.to_string(),
            asset_id: None,
        });
        total_distributed += *amount;
        tracing::info!(
            "💰 Burn refund output: {} -> {} PMS",
            &wallet_address[..20.min(wallet_address.len())],
            amount
        );
    }

    // 2b. TREASURY TAX (First cut)
    let treasury_percent = Decimal::from(settings.fees.treasury_fee_percent);
    let mut node_pool_amount = total_node_fees;

    if treasury_percent > Decimal::ZERO && total_node_fees > Decimal::ZERO {
        let treasury_cut = (total_node_fees * treasury_percent / Decimal::from(100)).round_dp(8);
        if treasury_cut > Decimal::ZERO {
            // Get treasury wallets: prefer loaded file, fallback to config
            let treasury_wallets = if !state.treasury_wallets.is_empty() {
                &state.treasury_wallets.list
            } else {
                &settings.fees.treasury_addresses
            };

            if !treasury_wallets.is_empty() {
                // Pick random treasury wallet or first one
                let target = &treasury_wallets[0];
                all_outputs.push(TxOutput {
                    address: target.clone(),
                    amount: treasury_cut.to_string(),
                    asset_id: None,
                });
                total_distributed += treasury_cut;
                node_pool_amount -= treasury_cut;
                tracing::info!(
                    "🏛️ Treasury Tax ({}%): {} PMS -> {}",
                    settings.fees.treasury_fee_percent,
                    treasury_cut,
                    &target[..20.min(target.len())]
                );
            } else {
                // [FALLBACK SÉCURITÉ] Pas de treasury wallet → On laisse les fonds dans le pool pour les Nœuds/Créateur
                // On ne déduit PAS `treasury_cut` de `node_pool_amount`.
                tracing::warn!(
                    "⚠️ Treasury tax enabled but no treasury addresses configured! Keeping {} PMS in node pool distribution (fallback to nodes).",
                    treasury_cut
                );
            }
        }
    }

    // 2c. NODE FEES (Remaining amount distributed by share)
    if node_pool_amount > Decimal::ZERO {
        let registry = state.node_registry.read().await;
        let nodes = registry.get_active_nodes();

        for (node_pk, share_pct, _original_share_amount) in &shares {
            // Recalculate share amount based on remaining pool
            let share_amount = (node_pool_amount * *share_pct).round_dp(8);

            if share_amount <= Decimal::ZERO {
                continue;
            }

            // Find node info to get wallet address
            let node_info = nodes.iter().find(|n| &n.node_pk == node_pk);
            let mut target_address = None;

            if let Some(node) = node_info {
                if let Some(addr) = &node.wallet_address {
                    target_address = Some(addr.clone());
                } else {
                    tracing::warn!(
                        "⚠️ Node {} has no registered wallet address!",
                        &node_pk[..10]
                    );
                }
            } else {
                tracing::warn!(
                    "⚠️ Node {} disappeared from registry during distribution!",
                    &node_pk[..10]
                );
            }

            // If no target address found (node missing or no wallet), fallback to Treasury
            if target_address.is_none() {
                // Determine fallback treasury address (same logic as tax)
                let fallback = if !state.treasury_wallets.is_empty() {
                    Some(state.treasury_wallets.list[0].clone())
                } else if !settings.fees.treasury_addresses.is_empty() {
                    Some(settings.fees.treasury_addresses[0].clone())
                } else {
                    None
                };

                if let Some(addr) = fallback {
                    tracing::warn!(
                        "⚠️ Redirecting {} PMS for node {} to Treasury (fallback)",
                        share_amount,
                        &node_pk[..10]
                    );
                    target_address = Some(addr);
                }
            }

            if let Some(addr) = target_address {
                all_outputs.push(TxOutput {
                    address: addr.clone(),
                    amount: share_amount.to_string(),
                    asset_id: None,
                });
                total_distributed += share_amount;
                tracing::info!(
                    "👷 Node Reward: {} PMS -> {} (Node: {})",
                    share_amount,
                    &addr[..20.min(addr.len())],
                    &node_pk[..10]
                );
            } else {
                tracing::error!(
                    "❌ FAILED to distribute {} PMS for Node {}: No wallet & No Treasury fallback!",
                    share_amount,
                    &node_pk[..10]
                );
            }
        }
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

    // 3. CREATE MINT BLOCK
    let mint_payload = PlainPayload::Mint {
        outputs: all_outputs.clone(),
    };

    let mut reward_block = Block {
        id: String::new(),
        parents: vec![parent_id.clone()],
        payload: Some(PayloadEnvelope::Plain(mint_payload)),
        nonce: 0,
        metadata: Some(pms_types_block::BlockMetadata {
            signer_x25519_hex: Some(coordinator_x25519.clone()),
            description: Some(format!(
                "Fees/Refunds: {} PMS to {} wallets",
                total_distributed, num_recipients
            )),
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

    // PoW
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

    // Build WireBlock
    let reward_payload_json = serde_json::to_string(&reward_block.payload)?;
    let mut reward_wb = WireBlock {
        id: reward_block.id.clone(),
        parents: reward_block.parents.clone(),
        payload_json: Some(reward_payload_json),
        nonce: reward_block.nonce,
        network_id: state._cfg.network.network_id.clone(),
        protocol_version: state._cfg.network.protocol_version as u16,
        signer_pk_hex: node_wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: reward_block.metadata.clone(),
    };

    // Sign
    let reward_msg = canonical_wireblock_message(&reward_wb);
    reward_wb.signature_hex = node_wallet.sign(&reward_msg)?;

    // 4. PERSIST
    match state.srv.adapter_arc().persist_block(&reward_wb).await {
        Ok(PutResult::Inserted) => {
            let _ = state.srv.enqueue_broadcast(reward_wb.id.clone()).await;

            // 5. UPDATE UTXOS DIRECTLY
            for (idx, output) in all_outputs.iter().enumerate() {
                state
                    .srv
                    .adapter_arc()
                    .add_utxo(
                        reward_wb.id.clone(),
                        idx as u32,
                        output.address.clone(),
                        output.amount.clone(),
                        None, // rewards always PMS
                    )
                    .await;
            }

            // 6. RESET POOL
            {
                let mut pool = state.fee_pool.write().await;
                pool.reset();
            }

            tracing::info!(
                "📦 Automated fees distributed: {} PMS to {} wallets (block: {})",
                total_distributed,
                num_recipients,
                &reward_wb.id[..16]
            );

            Ok(DistributeFeesResult {
                success: true,
                reward_block_id: Some(reward_wb.id),
                total_distributed: total_distributed.to_string(),
                num_recipients,
            })
        }
        Ok(PutResult::AlreadyExists) => {
            // Should not happen with nonce increment, but possible
            anyhow::bail!("Reward block already exists")
        }
        Ok(PutResult::Rejected(r)) => {
            anyhow::bail!("Reward block rejected: {}", r)
        }
        Err(e) => anyhow::bail!("Storage error: {}", e),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Scheduled Daily Inflation Mint
// ═══════════════════════════════════════════════════════════════════════════════

/// Exécute un mint d'inflation quotidien basé sur le supply en circulation.
/// daily_amount = circulating_supply * annual_inflation_percent / 365
/// Distribué selon creator_reward_percent / treasury_reward_percent / burn_percent.
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
    let treasury_addr = if !state.treasury_wallets.is_empty() {
        state.treasury_wallets.list[0].clone()
    } else if !settings.fees.treasury_addresses.is_empty() {
        settings.fees.treasury_addresses[0].clone()
    } else {
        coordinator_address.clone()
    };

    let mut all_outputs: Vec<TxOutput> = Vec::new();
    let mut total_distributed = Decimal::ZERO;

    if coordinator_amount > Decimal::ZERO {
        all_outputs.push(TxOutput {
            address: coordinator_address.clone(),
            amount: coordinator_amount.normalize().to_string(),
            asset_id: None,
        });
        total_distributed += coordinator_amount;
    }

    if treasury_amount > Decimal::ZERO {
        all_outputs.push(TxOutput {
            address: treasury_addr.clone(),
            amount: treasury_amount.normalize().to_string(),
            asset_id: None,
        });
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
        network_id: state._cfg.network.network_id.clone(),
        protocol_version: state._cfg.network.protocol_version as u16,
        signer_pk_hex: node_wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: block.metadata.clone(),
    };

    let msg = canonical_wireblock_message(&wb);
    wb.signature_hex = node_wallet.sign(&msg)?;

    // 7. PERSIST & UPDATE UTXOs
    match state.srv.adapter_arc().persist_block(&wb).await {
        Ok(PutResult::Inserted) => {
            let _ = state.srv.enqueue_broadcast(wb.id.clone()).await;

            for (idx, output) in all_outputs.iter().enumerate() {
                state
                    .srv
                    .adapter_arc()
                    .add_utxo(
                        wb.id.clone(),
                        idx as u32,
                        output.address.clone(),
                        output.amount.clone(),
                        None, // inflation always PMS
                    )
                    .await;
            }

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

// ═══════════════════════════════════════════════════════════════════════════════
// Unit Tests
// ═══════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod tests {
    use super::*;

    // ═══════════════════════════════════════════════════════════════════════
    // Tests pour FeeDistributionConfig
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn fee_distribution_config_default_sums_to_100() {
        let config = FeeDistributionConfig::default();
        assert_eq!(config.coordinator_percent, 65);
        assert_eq!(config.treasury_percent, 35);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn fee_distribution_config_validation_rejects_invalid_sum() {
        let config = FeeDistributionConfig::new(60, 50); // = 110
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must sum to 100"));
    }

    #[test]
    fn fee_distribution_config_accepts_valid_custom() {
        let config = FeeDistributionConfig::new(70, 30);
        assert!(config.validate().is_ok());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Tests pour BlockRewardConfig
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn block_reward_config_default() {
        let config = BlockRewardConfig::default();
        assert_eq!(config.reward_per_block, "0.1");
        assert_eq!(config.creator_percent, 70);
        assert_eq!(config.treasury_percent, 20);
        assert_eq!(config.burn_percent, 10);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Tests pour compute_block_reward_outputs
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn compute_block_reward_outputs_splits_correctly() {
        let config = BlockRewardConfig::default(); // 0.1 PMS, 70/20/10
        let creator_addr = "creator_address_123";
        let treasury_addr = "treasury_address_456";

        let (outputs, burn) = compute_block_reward_outputs(creator_addr, treasury_addr, &config);

        // Vérifie qu'on a 2 outputs (creator + treasury)
        assert_eq!(outputs.len(), 2);

        // Creator devrait recevoir 70% de 0.1 = 0.07
        let creator_output = outputs.iter().find(|o| o.address == creator_addr);
        assert!(creator_output.is_some());
        let creator_amount: Decimal = creator_output.unwrap().amount.parse().unwrap();
        assert_eq!(creator_amount, "0.07".parse::<Decimal>().unwrap());

        // Treasury devrait recevoir 20% de 0.1 = 0.02
        let treasury_output = outputs.iter().find(|o| o.address == treasury_addr);
        assert!(treasury_output.is_some());
        let treasury_amount: Decimal = treasury_output.unwrap().amount.parse().unwrap();
        assert_eq!(treasury_amount, "0.02".parse::<Decimal>().unwrap());

        // Burn devrait être 10% de 0.1 = 0.01
        assert_eq!(burn, "0.01".parse::<Decimal>().unwrap());
    }

    #[test]
    fn compute_block_reward_outputs_zero_reward() {
        let config = BlockRewardConfig {
            reward_per_block: "0".to_string(),
            ..Default::default()
        };

        let (outputs, burn) = compute_block_reward_outputs("creator", "treasury", &config);

        assert!(outputs.is_empty());
        assert_eq!(burn, Decimal::ZERO);
    }

    #[test]
    fn compute_block_reward_outputs_custom_percentages() {
        let config = BlockRewardConfig {
            reward_per_block: "1.0".to_string(),
            creator_percent: 50,
            treasury_percent: 30,
            burn_percent: 20,
        };

        let (outputs, burn) = compute_block_reward_outputs("c", "t", &config);

        assert_eq!(outputs.len(), 2);

        // Creator: 50% de 1.0 = 0.5
        let creator_amount: Decimal = outputs[0].amount.parse().unwrap();
        assert_eq!(creator_amount, "0.5".parse::<Decimal>().unwrap());

        // Treasury: 30% de 1.0 = 0.3
        let treasury_amount: Decimal = outputs[1].amount.parse().unwrap();
        assert_eq!(treasury_amount, "0.3".parse::<Decimal>().unwrap());

        // Burn: 20% de 1.0 = 0.2
        assert_eq!(burn, "0.2".parse::<Decimal>().unwrap());
    }
}
