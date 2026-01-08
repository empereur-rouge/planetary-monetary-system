use anyhow::{Result, bail};
use pms_config::{FeePickMode, Settings};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::sync::atomic::{AtomicUsize, Ordering};

pub static ROUND_ROBIN_IDX: AtomicUsize = AtomicUsize::new(0);

/// Choisit une adresse admin pour recevoir les fees.
/// - Uniform: random uniforme
/// - RoundRobin: rotation simple (thread-safe)
pub fn pick_fee_recipient_address(settings: &Settings) -> Result<String> {
    // NOTE: if-let combiné pour satisfaire clippy::collapsible_if
    // En mode Dev, on peut override l'adresse fee via variable d'env pour les tests
    if matches!(settings.network.mode, pms_config::NetworkMode::Dev)
        && let Ok(addr) = std::env::var("PMS_TEST_FEE_ADDR")
    {
        let addr = addr.trim().to_string();
        if !addr.is_empty() {
            return Ok(addr);
        }
    }

    let admins = &settings.admin.wallet_addresses;
    if admins.is_empty() {
        bail!("fees.wallet_addresses is empty");
    }

    match settings.fees.mode {
        FeePickMode::Uniform => {
            // ------------------------------------------------------------
            // Random uniforme (optionnellement seedé)
            // ------------------------------------------------------------
            //
            // En prod: seed=None => RNG système
            // En tests: seed=Some(x) => déterministe
            let idx = if let Some(seed) = settings.fees.seed {
                let mut rng = StdRng::seed_from_u64(seed);
                rng.random_range(0..admins.len())
            } else {
                rand::rng().random_range(0..admins.len())
            };

            Ok(admins[idx].clone())
        }

        FeePickMode::RoundRobin => {
            // ------------------------------------------------------------
            // Round-robin: 0,1,2,0,1,2...
            // ------------------------------------------------------------
            let i = ROUND_ROBIN_IDX.fetch_add(1, Ordering::Relaxed);
            Ok(admins[i % admins.len()].clone())
        }
    }
}
