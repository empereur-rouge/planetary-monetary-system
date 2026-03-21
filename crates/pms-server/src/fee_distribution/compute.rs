use pms_config::FeeDistributionConfig;
use rust_decimal::Decimal;

/// Represents a fee output to include in a block.
#[derive(Debug, Clone)]
pub struct FeeOutput {
    pub address: String,
    pub amount: String,
}

/// Resolves the address of a beneficiary based on their role.
pub(super) fn resolve_beneficiary_address(
    beneficiary: &pms_config::FeeBeneficiary,
    coordinator_address: &str,
    treasury_addresses: &[String],
) -> Option<String> {
    // If an explicit address is defined, use it
    if let Some(addr) = &beneficiary.address {
        return Some(addr.clone());
    }
    // Resolution by role
    match beneficiary.role.as_str() {
        "coordinator" => Some(coordinator_address.to_string()),
        "treasury" => {
            if treasury_addresses.is_empty() {
                eprintln!("[FEE] Warning: No treasury wallet configured. Fallback to coordinator.");
                Some(coordinator_address.to_string())
            } else {
                use rand::Rng;
                let idx = rand::rng().random_range(0..treasury_addresses.len());
                Some(treasury_addresses[idx].clone())
            }
        }
        _ => {
            // Custom roles (client, partner...): address is required
            eprintln!(
                "[FEE] Warning: Beneficiary role '{}' has no address, skipping.",
                beneficiary.role
            );
            None
        }
    }
}

/// Calculates the fee outputs for a transaction (N-way split).
///
/// # Arguments
/// * `total_fee` - The total fee amount
/// * `treasury_addresses` - List of treasury addresses
/// * `coordinator_address` - Coordinator address
/// * `config` - N-way distribution config
///
/// # Returns
/// List of FeeOutput to include in the payload
pub fn compute_fee_outputs(
    total_fee: Decimal,
    treasury_addresses: &[String],
    coordinator_address: &str,
    config: &FeeDistributionConfig,
) -> Vec<FeeOutput> {
    if let Err(e) = config.validate() {
        eprintln!("[FEE] Config validation error: {}, using defaults", e);
    }

    let mut outputs = Vec::new();
    for beneficiary in &config.beneficiaries {
        let amount =
            (total_fee * Decimal::from(beneficiary.percent_bps) / Decimal::from(10000)).round_dp(8);
        if amount <= Decimal::ZERO {
            continue;
        }
        if let Some(addr) =
            resolve_beneficiary_address(beneficiary, coordinator_address, treasury_addresses)
        {
            outputs.push(FeeOutput {
                address: addr,
                amount: amount.normalize().to_string(),
            });
        }
    }
    outputs
}
