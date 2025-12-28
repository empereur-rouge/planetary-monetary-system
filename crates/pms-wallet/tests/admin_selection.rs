use pms_wallet::{pick_index_from_balances};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use rand::prelude::*;
use anyhow::Result;

#[test]
fn weighted_selection_prefers_low_balance_real() -> Result<()> {
    // LOW = 0, MID = 100, HIGH = 300
    let balances = vec![dec!(0), dec!(100), dec!(300)];
    let epsilon = dec!(0.001);

    let mut rng = StdRng::seed_from_u64(42);

    let mut low = 0;
    let mut mid = 0;
    let mut high = 0;

    for _ in 0..1_000 {
        let idx = pick_index_from_balances(&balances, epsilon, &mut rng)?;
        match idx {
            0 => low += 1,
            1 => mid += 1,
            2 => high += 1,
            _ => unreachable!(),
        }
    }

    // On s’attend à ce que LOW soit choisi plus souvent que HIGH
    assert!(
        low > high,
        "LOW must be chosen more often: low={low}, mid={mid}, high={high}"
    );

    Ok(())
}