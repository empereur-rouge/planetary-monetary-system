use anyhow::Result;
use pms_ledger::{ROUND_ROBIN_IDX, pick_fee_recipient_address};

#[test]
fn fee_recipient_uniform_is_always_in_admin_list() -> anyhow::Result<()> {
    let mut s = pms_config::load_config()?;
    s.admin.wallet_addresses = vec!["A".into(), "B".into(), "C".into()];
    s.fees.mode = pms_config::FeePickMode::Uniform;
    s.fees.seed = Some(42);

    for _ in 0..200 {
        let pick = pick_fee_recipient_address(&s)?;
        assert!(s.admin.wallet_addresses.contains(&pick));
    }
    Ok(())
}

#[test]
fn fee_recipient_round_robin_cycles() -> anyhow::Result<()> {
    let mut s = pms_config::load_config()?;
    s.admin.wallet_addresses = vec!["A".into(), "B".into(), "C".into()];
    s.fees.mode = pms_config::FeePickMode::RoundRobin;
    s.fees.seed = None;

    // reset statique si tu veux un test isolé :
    // (si tu veux éviter ça, on peut sortir ROUND_ROBIN_IDX dans un struct instancié)
    ROUND_ROBIN_IDX.store(0, std::sync::atomic::Ordering::Relaxed);

    let picks: Vec<String> = (0..7)
        .map(|_| pick_fee_recipient_address(&s))
        .collect::<Result<_>>()?;

    assert_eq!(picks, vec!["A", "B", "C", "A", "B", "C", "A"]);
    Ok(())
}
