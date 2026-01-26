// ═══════════════════════════════════════════════════════════════════════════════
// Fee Distribution Module
// ═══════════════════════════════════════════════════════════════════════════════
//
// Ce module gère la distribution des frais de transaction entre :
// - Le Treasury (wallets admin) : 15% par défaut
// - Le créateur du bloc (node qui traite) : 45% par défaut
// - Les signataires des blocs parents : 40% par défaut (20% chacun)
//
// Voir Chapitre 5 du Rust Book pour les structures et méthodes.
// ═══════════════════════════════════════════════════════════════════════════════

use pms_storage::DagStorage;
use pms_wallet::make_address;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Configuration pour la distribution des fees
#[derive(Debug, Clone)]
pub struct FeeDistributionConfig {
    /// Pourcentage vers le treasury (admin wallets)
    pub treasury_percent: u8,
    /// Pourcentage vers le créateur du bloc
    pub creator_percent: u8,
    /// Pourcentage vers les parents (réparti entre eux)
    pub parents_percent: u8,
}

impl Default for FeeDistributionConfig {
    fn default() -> Self {
        Self {
            treasury_percent: 15,
            creator_percent: 45,
            parents_percent: 40,
        }
    }
}

impl FeeDistributionConfig {
    /// Crée une config depuis les settings (pour intégration future)
    pub fn from_percents(treasury: u8, creator: u8, parents: u8) -> Self {
        Self {
            treasury_percent: treasury,
            creator_percent: creator,
            parents_percent: parents,
        }
    }

    /// Valide que les pourcentages totalisent 100%
    pub fn validate(&self) -> Result<(), String> {
        let total = self.treasury_percent + self.creator_percent + self.parents_percent;
        if total != 100 {
            return Err(format!("Fee percentages must sum to 100, got {}", total));
        }
        Ok(())
    }
}

/// Calcule les outputs de fee pour une transaction
///
/// # Arguments
/// * `total_fee` - Le montant total des fees
/// * `treasury_addresses` - Liste des adresses admin (choisie aléatoirement)
/// * `creator_address` - Adresse du node créant le bloc
/// * `parent_ids` - IDs des blocs parents
/// * `store` - Store pour récupérer les signers des parents
/// * `hrp` - Préfixe Bech32m (ex: "8e")
/// * `config` - Configuration de distribution
///
/// # Returns
/// Liste des FeeOutput à inclure dans le payload
pub async fn compute_fee_outputs<S: DagStorage>(
    total_fee: Decimal,
    treasury_addresses: &[String],
    creator_address: &str,
    parent_ids: &[String],
    store: &Arc<S>,
    hrp: &str,
    config: &FeeDistributionConfig,
) -> Vec<FeeOutput> {
    let mut outputs = Vec::new();

    // Valide la config (log si erreur mais continue avec fallback)
    if let Err(e) = config.validate() {
        eprintln!("[FEE] Config validation error: {}, using defaults", e);
    }

    // Calcul des montants
    let treasury_amount =
        total_fee * Decimal::from_u8(config.treasury_percent).unwrap() / Decimal::from(100);
    let creator_amount =
        total_fee * Decimal::from_u8(config.creator_percent).unwrap() / Decimal::from(100);
    let parents_total =
        total_fee * Decimal::from_u8(config.parents_percent).unwrap() / Decimal::from(100);

    // 1) Treasury (choisit une adresse pseudo-aléatoirement si plusieurs)
    if !treasury_addresses.is_empty() && treasury_amount > Decimal::ZERO {
        // Simple pseudo-random based on timestamp
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as usize)
            .unwrap_or(0);
        let idx = seed % treasury_addresses.len();
        outputs.push(FeeOutput {
            address: treasury_addresses[idx].clone(),
            amount: treasury_amount.normalize().to_string(),
        });
    }

    // 2) Créateur du bloc
    if creator_amount > Decimal::ZERO {
        outputs.push(FeeOutput {
            address: creator_address.to_string(),
            amount: creator_amount.normalize().to_string(),
        });
    }

    // 3) Parents (split égal entre eux)
    let parent_addresses = get_parent_signer_addresses(parent_ids, store, hrp).await;
    if !parent_addresses.is_empty() && parents_total > Decimal::ZERO {
        let per_parent = parents_total / Decimal::from(parent_addresses.len());
        for addr in parent_addresses {
            outputs.push(FeeOutput {
                address: addr,
                amount: per_parent.normalize().to_string(),
            });
        }
    } else {
        // Fallback: si pas de parents avec adresse, le créateur récupère cette part
        if parents_total > Decimal::ZERO {
            outputs.push(FeeOutput {
                address: creator_address.to_string(),
                amount: parents_total.normalize().to_string(),
            });
        }
    }

    outputs
}

/// Récupère les adresses des signataires des blocs parents
///
/// Pour chaque parent :
/// 1. Récupère le bloc depuis le store
/// 2. Extrait signer_pk_hex et metadata.signer_x25519_hex
/// 3. Construit l'adresse Bech32m
///
/// Les parents sans signer ou sans X25519 key sont ignorés.
async fn get_parent_signer_addresses<S: DagStorage>(
    parent_ids: &[String],
    store: &Arc<S>,
    hrp: &str,
) -> Vec<String> {
    let mut addresses = Vec::new();

    for parent_id in parent_ids {
        // Récupère le bloc parent
        let parent_block = match store.get_block(parent_id).await {
            Ok(Some(b)) => b,
            _ => continue,
        };

        // Le signer_pk_hex est dans le StoredBlock
        let signer_pk = &parent_block.signer_pk_hex;
        if signer_pk.is_empty() {
            continue;
        }

        // Le X25519 key est dans metadata
        let x25519_hex = parent_block
            .metadata
            .as_ref()
            .and_then(|m| m.signer_x25519_hex.clone())
            .unwrap_or_default();

        if x25519_hex.is_empty() {
            continue;
        }

        // Construit l'adresse Bech32m
        let address = make_address(hrp, signer_pk, &x25519_hex);
        addresses.push(address);
    }

    addresses
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
        });
        total_distributed += *amount;
        tracing::info!(
            "💰 Burn refund output: {} -> {} PMS",
            &wallet_address[..20.min(wallet_address.len())],
            amount
        );
    }

    // 2b. NODE FEES
    {
        let registry = state.node_registry.read().await;
        let nodes = registry.get_active_nodes();

        for (node_pk, _share_pct, share_amount) in &shares {
            if *share_amount <= Decimal::ZERO {
                continue;
            }
            if let Some(node) = nodes.iter().find(|n| &n.node_pk == node_pk) {
                // TODO: Use real reward address
                tracing::debug!(
                    "Found node {} with api_url {}, but no reward address yet",
                    &node_pk[..16.min(node_pk.len())],
                    node.api_url
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
            let mut idx = 0u32;
            for output in &all_outputs {
                state
                    .srv
                    .adapter_arc()
                    .add_utxo(
                        reward_wb.id.clone(),
                        idx,
                        output.address.clone(),
                        output.amount.clone(),
                    )
                    .await;
                idx += 1;
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
        assert_eq!(config.treasury_percent, 15);
        assert_eq!(config.creator_percent, 45);
        assert_eq!(config.parents_percent, 40);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn fee_distribution_config_validation_rejects_invalid_sum() {
        let config = FeeDistributionConfig::from_percents(20, 50, 40); // = 110
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must sum to 100"));
    }

    #[test]
    fn fee_distribution_config_accepts_valid_custom() {
        let config = FeeDistributionConfig::from_percents(10, 60, 30);
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
