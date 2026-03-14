use pms_types_economics::FeeBurnResult;
use rust_decimal::Decimal;

/// Calculate how much of the total fees should be burned vs distributed.
///
/// `burn_rate_bps` is in basis points: 3000 = 30%, 5000 = 50%, 0 = no burn.
/// Capped at 10_000 (100%) — though burning 100% is economically silly.
///
/// # Examples
/// ```
/// use pms_economics::fee_burn::calculate_fee_burn;
/// use rust_decimal_macros::dec;
///
/// let result = calculate_fee_burn(dec!(100), 3000);
/// assert_eq!(result.burned, dec!(30));
/// assert_eq!(result.distributable, dec!(70));
/// ```
pub fn calculate_fee_burn(total_fees: Decimal, burn_rate_bps: u32) -> FeeBurnResult {
    if burn_rate_bps == 0 || total_fees.is_zero() {
        return FeeBurnResult {
            burned: Decimal::ZERO,
            distributable: total_fees,
        };
    }

    let rate = Decimal::from(burn_rate_bps.min(10_000)) / Decimal::from(10_000u32);
    let burned = (total_fees * rate).round_dp(8);
    let distributable = total_fees - burned;

    FeeBurnResult {
        burned,
        distributable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn burn_30_percent() {
        let r = calculate_fee_burn(dec!(100), 3000);
        println!("burned={} distributable={}", r.burned, r.distributable);
        assert_eq!(r.burned, dec!(30));
        assert_eq!(r.distributable, dec!(70));
    }

    #[test]
    fn burn_50_percent() {
        let r = calculate_fee_burn(dec!(1.00), 5000);
        println!("burned={} distributable={}", r.burned, r.distributable);
        assert_eq!(r.burned, dec!(0.50));
        assert_eq!(r.distributable, dec!(0.50));
    }

    #[test]
    fn burn_zero_rate() {
        let r = calculate_fee_burn(dec!(100), 0);
        println!("burned={} distributable={}", r.burned, r.distributable);
        assert_eq!(r.burned, dec!(0));
        assert_eq!(r.distributable, dec!(100));
    }

    #[test]
    fn burn_zero_fees() {
        let r = calculate_fee_burn(dec!(0), 3000);
        println!("burned={} distributable={}", r.burned, r.distributable);
        assert_eq!(r.burned, dec!(0));
        assert_eq!(r.distributable, dec!(0));
    }

    #[test]
    fn burn_100_percent_capped() {
        let r = calculate_fee_burn(dec!(50), 10_000);
        println!("burned={} distributable={}", r.burned, r.distributable);
        assert_eq!(r.burned, dec!(50));
        assert_eq!(r.distributable, dec!(0));
    }

    #[test]
    fn burn_rate_above_10000_capped() {
        let r = calculate_fee_burn(dec!(50), 15_000);
        println!("burned={} distributable={}", r.burned, r.distributable);
        // Capped at 100%
        assert_eq!(r.burned, dec!(50));
        assert_eq!(r.distributable, dec!(0));
    }

    #[test]
    fn burn_small_amount_precision() {
        let r = calculate_fee_burn(dec!(0.00000123), 3000);
        println!("burned={} distributable={}", r.burned, r.distributable);
        // Should not lose precision (8 decimal places)
        assert_eq!(r.burned + r.distributable, dec!(0.00000123));
    }

    #[test]
    fn burn_10_percent() {
        let r = calculate_fee_burn(dec!(100), 1000);
        println!("burned={} distributable={}", r.burned, r.distributable);
        assert_eq!(r.burned, dec!(10));
        assert_eq!(r.distributable, dec!(90));
    }
}
