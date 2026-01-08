use anyhow::Result;
use futures::future::join_all;
use rand::distr::Distribution;
use rand::distr::weighted::WeightedIndex;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
// ----- Importe tes fonctions réelles -----
use pms_wallet::{Wallet, decode_address, pick_admin_address_weighted, pick_admin_recipient};

// ---------------- Test helpers (déterministes) ----------------

/// Recalcule les poids comme en prod: max(avg - bal, 0) + epsilon
fn compute_weights(balances: &[Decimal], epsilon: Decimal) -> Vec<f64> {
    let sum: Decimal = balances.iter().copied().sum();
    let n = Decimal::from(balances.len() as u64);
    let avg = if n.is_zero() { Decimal::ZERO } else { sum / n };

    balances
        .iter()
        .map(|b| {
            let need = (avg - *b).max(Decimal::ZERO) + epsilon;
            need.to_f64().unwrap_or(0.0).max(0.0)
        })
        .collect()
}

/// Tire au sort N fois de manière déterministe avec un RNG seedé.
fn sample_counts(weights: &[f64], trials: usize, seed: u64) -> Vec<usize> {
    let mut dist = WeightedIndex::new(weights).expect("weights invalid");
    let mut rng = StdRng::seed_from_u64(seed);
    let mut counts = vec![0usize; weights.len()];
    for _ in 0..trials {
        let i = dist.sample(&mut rng);
        counts[i] += 1;
    }
    counts
}

// ------------------------------ TESTS ------------------------------

#[tokio::test]
async fn empty_list_errors() {
    let addrs: Vec<String> = vec![];
    let epsilon = Decimal::new(1, 3); // 0.001
    let fetch = |_a: &str| async { Ok(Decimal::ZERO) };

    let err = pick_admin_address_weighted(&addrs, fetch, epsilon)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("admin.wallet_addresses est vide"));
}

#[tokio::test]
async fn uniform_balances_are_near_uniform() -> Result<()> {
    let addrs: Vec<String> = vec!["A".into(), "B".into(), "C".into()];
    let epsilon = Decimal::new(1, 3); // 0.001
    // balances uniformes: 10, 10, 10
    let balances = vec![dec!(10), dec!(10), dec!(10)];
    let ws = compute_weights(&balances, epsilon);
    // somme non nulle, quasi uniforme
    let counts = sample_counts(&ws, 20_000, 42);

    // Chaque bin doit être proche de 1/3 (~6666). Tolérance ±8%.
    for c in counts {
        assert!((c as i64 - 6666).abs() < 540, "counts too skewed: {:?}", c);
    }
    Ok(())
}

#[tokio::test]
async fn underfunded_gets_higher_prob() -> Result<()> {
    let addrs: Vec<String> = vec!["A".into(), "B".into(), "C".into()];
    // ε assez grand pour obtenir des sélections visibles
    let epsilon = dec!(1); // 1.0

    let balances = vec![dec!(0.000002), dec!(100.0000001), dec!(100.1)];
    let ws = compute_weights(&balances, epsilon);
    let counts = sample_counts(&ws, 20_000, 7);

    assert!(
        counts[0] > counts[1] && counts[0] > counts[2],
        "expected A most selected, got {:?}",
        counts
    );
    assert!(
        counts[1] > 0 && counts[2] > 0,
        "epsilon too small? counts={:?}",
        counts
    );
    Ok(())
}

#[tokio::test]
async fn epsilon_prevents_zero_probability() -> Result<()> {
    let addrs: Vec<String> = vec!["A".into(), "B".into()];
    // balances égales et très élevées → poids = epsilon pour tous
    let balances = vec![dec!(1_000_000), dec!(1_000_000)];

    // epsilon très petit mais > 0
    let epsilon = Decimal::new(1, 6); // 0.000001
    let ws = compute_weights(&balances, epsilon);
    let counts = sample_counts(&ws, 10_000, 123);

    // Les deux reçoivent des sélections
    assert!(counts[0] > 0 && counts[1] > 0);
    Ok(())
}

#[tokio::test]
async fn pick_admin_address_weighted_matches_manual_sampling() -> Result<()> {
    let addrs = vec!["A".into(), "B".into(), "C".into()];
    let epsilon = Decimal::new(1, 3);

    // balances: 5, 10, 15  -> déficit relatif croissant de A vers C ? (avg=10)
    // need(A)=5+eps; need(B)=0+eps; need(C)=0 (clamp)+eps
    let balances = vec![dec!(5), dec!(10), dec!(15)];
    let ws = compute_weights(&balances, epsilon);

    // On appelle la vraie fonction N fois avec un fetch_balance qui renvoie ces soldes.
    let trials = 12_000usize;
    let mut counts = vec![0usize; 3];
    for _ in 0..trials {
        let addr = pick_admin_address_weighted(
            &addrs,
            |a: &str| {
                let b = match a {
                    "A" => dec!(5),
                    "B" => dec!(10),
                    "C" => dec!(15),
                    _ => dec!(0),
                };
                async move { Ok(b) }
            },
            epsilon,
        )
        .await?;
        match addr.as_str() {
            "A" => counts[0] += 1,
            "B" => counts[1] += 1,
            "C" => counts[2] += 1,
            _ => {}
        }
    }

    // La tendance doit respecter ws: A > B ≈ C (car B/C n’ont que epsilon)
    assert!(
        counts[0] > counts[1] && counts[0] > counts[2],
        "counts={:?}",
        counts
    );
    Ok(())
}

#[tokio::test]
async fn pick_admin_recipient_returns_xpk_and_matches_wallet_encoding() -> Result<()> {
    // Génère 2 wallets, extrait leurs adresses Bech32m, puis vérifie que la X25519 extraite correspond.
    let settings = pms_config::load_config()?;
    let (w1, w2) = (Wallet::generate(), Wallet::generate());
    let a1 = w1.get_address(&settings.address.hrp);
    let a2 = w2.get_address(&settings.address.hrp);

    // Sanity: decode_address doit retrouver la X25519 incluse
    let (_h1, x1) = decode_address(&a1).expect("decode a1");
    assert_eq!(x1, w1.x25519_pub_hex);
    let (_h2, x2) = decode_address(&a2).expect("decode a2");
    assert_eq!(x2, w2.x25519_pub_hex);

    // Liste d’adresses admin
    let admin_addrs = vec![a1.clone(), a2.clone()];

    // Stub: soldes = 0 -> sélection approx uniforme, mais on teste seulement que ça renvoie une XPK valide
    let xpk = pick_admin_recipient(&admin_addrs).await?;
    assert!(!xpk.is_empty());
    assert_eq!(
        xpk.len(),
        64,
        "X25519 hex length should be 64, got {}",
        xpk.len()
    );
    Ok(())
}
