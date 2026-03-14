use rust_decimal::Decimal;

/// Calculate storage fee based on payload size and per-KB rate.
///
/// Formula: `ceil(payload_bytes / 1024) * fee_per_kb`
/// Minimum 1 KB unit — even a 1-byte payload costs 1 KB worth of fee.
/// Returns `Decimal::ZERO` if `fee_per_kb` is zero or payload is empty.
///
/// # Examples
/// ```
/// use pms_economics::storage_fee::calculate_storage_fee;
/// use rust_decimal_macros::dec;
///
/// // 2048 bytes = 2 KB → 2 * 0.01 = 0.02
/// assert_eq!(calculate_storage_fee(2048, dec!(0.01)), dec!(0.02));
/// ```
pub fn calculate_storage_fee(payload_bytes: usize, fee_per_kb: Decimal) -> Decimal {
    if payload_bytes == 0 || fee_per_kb.is_zero() {
        return Decimal::ZERO;
    }
    // Ceiling division: (bytes + 1023) / 1024
    let kb_units = ((payload_bytes + 1023) / 1024) as u64;
    Decimal::from(kb_units) * fee_per_kb
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn zero_bytes_no_fee() {
        let fee = calculate_storage_fee(0, dec!(0.01));
        println!("fee={fee}");
        assert_eq!(fee, dec!(0));
    }

    #[test]
    fn zero_rate_no_fee() {
        let fee = calculate_storage_fee(5000, dec!(0));
        println!("fee={fee}");
        assert_eq!(fee, dec!(0));
    }

    #[test]
    fn one_byte_costs_one_kb() {
        let fee = calculate_storage_fee(1, dec!(0.01));
        println!("fee={fee} (1 byte → 1 KB unit)");
        assert_eq!(fee, dec!(0.01));
    }

    #[test]
    fn exact_1kb() {
        let fee = calculate_storage_fee(1024, dec!(0.01));
        println!("fee={fee}");
        assert_eq!(fee, dec!(0.01));
    }

    #[test]
    fn just_over_1kb() {
        let fee = calculate_storage_fee(1025, dec!(0.01));
        println!("fee={fee} (1025 bytes → 2 KB units)");
        assert_eq!(fee, dec!(0.02));
    }

    #[test]
    fn large_nft_metadata() {
        // 50 KB metadata (rich NFT)
        let fee = calculate_storage_fee(50 * 1024, dec!(0.01));
        println!("fee={fee} (50 KB)");
        assert_eq!(fee, dec!(0.50));
    }

    #[test]
    fn typical_transaction() {
        // ~256 bytes typical tx → 1 KB unit
        let fee = calculate_storage_fee(256, dec!(0.01));
        println!("fee={fee}");
        assert_eq!(fee, dec!(0.01));
    }
}
