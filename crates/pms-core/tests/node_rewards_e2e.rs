//! Test d'intégration: flux complet des loyers de nœuds
//!
//! Ce test simule le cycle complet:
//! 1. Plusieurs nœuds minent des blocs
//! 2. Des transactions accumulent des fees dans le pool
//! 3. Un Milestone avec distribute_node_rewards: true déclenche la distribution

use anyhow::Result;
use pms_storage::NodeRewardsStorage;
use pms_testkit::test_rocks_store;

/// Test: Distribution complète via Milestone (simulation)
#[tokio::test]
async fn test_full_node_rewards_distribution() -> Result<()> {
    let tr = test_rocks_store("node_rewards_e2e").await?;
    let store = tr.store.clone();

    // Simuler 3 nœuds avec différentes clés publiques
    let pk_a = "pk_alice_1234567890abcdef";
    let pk_b = "pk_bob_fedcba0987654321";
    let pk_c = "pk_charlie_abcd1234efgh";

    // 1) Chaque nœud mine des blocs
    // Node A mine 5 blocs (50%)
    for _ in 0..5 {
        store.increment_node_block_count(pk_a)?;
    }
    // Node B mine 3 blocs (30%)
    for _ in 0..3 {
        store.increment_node_block_count(pk_b)?;
    }
    // Node C mine 2 blocs (20%)
    for _ in 0..2 {
        store.increment_node_block_count(pk_c)?;
    }

    // 2) Simuler des fees accumulées (normalement via TxUtxo)
    // Pool total: 10000 sats
    store.add_to_fee_pool(10000)?;

    // 3) Vérifier l'état avant distribution
    let pool = store.get_fee_pool()?;
    let miners = store.get_all_miners()?;
    let total_blocks: u64 = miners.iter().map(|(_, c)| *c).sum();

    assert_eq!(pool, 10000, "Pool should have 10000 sats");
    assert_eq!(total_blocks, 10, "Total blocks should be 10");
    assert_eq!(miners.len(), 3, "Should have 3 miners");

    // 4) Calculer les distributions
    let mut distributions = Vec::new();
    for (pk, count) in &miners {
        let share = pool * count / total_blocks;
        distributions.push((pk.clone(), share));
        println!("  {} : {} blocks → {} sats", &pk[..10], count, share);
    }

    // 5) Vérifier les parts attendues
    let total_distributed: u64 = distributions.iter().map(|(_, s)| *s).sum();
    assert_eq!(
        total_distributed, 10000,
        "Total distribution should equal pool"
    );

    // 6) Simuler le reset après distribution
    store.reset_pool_and_counts()?;

    // 7) Vérifier que tout est reset
    assert_eq!(store.get_fee_pool()?, 0, "Pool should be 0 after reset");
    assert_eq!(store.get_all_miners()?.len(), 0, "Miners should be empty");

    println!("✅ Distribution complète simulée avec succès!");

    Ok(())
}

/// Test: Le pool s'accumule correctement sur plusieurs transactions
#[tokio::test]
async fn test_pool_accumulates_from_transactions() -> Result<()> {
    let tr = test_rocks_store("node_rewards_tx_pool").await?;
    let store = tr.store.clone();

    // Simuler 5 transactions avec différentes fees
    // node_fee_bps = 3000 (30%)
    let node_fee_bps = 3000u64;

    let tx_fees = [100, 200, 150, 300, 250]; // sats
    for fee in tx_fees {
        let node_portion = fee * node_fee_bps / 10000;
        store.add_to_fee_pool(node_portion)?;
    }

    // Total fees: 100+200+150+300+250 = 1000
    // Node portion (30%): 300
    let expected_pool = 1000 * node_fee_bps / 10000;
    let actual_pool = store.get_fee_pool()?;

    assert_eq!(actual_pool, expected_pool, "Pool should match expected");
    println!("✅ Pool accumulated correctly: {} sats", actual_pool);

    Ok(())
}

/// Test: Plusieurs cycles de distribution
#[tokio::test]
async fn test_multiple_distribution_cycles() -> Result<()> {
    let tr = test_rocks_store("node_rewards_cycles").await?;
    let store = tr.store.clone();

    // === Cycle 1 ===
    store.add_to_fee_pool(5000)?;
    store.increment_node_block_count("miner_1")?;
    store.increment_node_block_count("miner_1")?;

    assert_eq!(store.get_fee_pool()?, 5000);
    assert_eq!(store.get_node_block_count("miner_1")?, 2);

    // Distribution simulée
    store.reset_pool_and_counts()?;

    // Vérifier reset
    assert_eq!(store.get_fee_pool()?, 0);
    assert_eq!(store.get_node_block_count("miner_1")?, 0);

    // === Cycle 2 ===
    store.add_to_fee_pool(8000)?;
    store.increment_node_block_count("miner_1")?;
    store.increment_node_block_count("miner_2")?;
    store.increment_node_block_count("miner_2")?;

    let miners = store.get_all_miners()?;
    assert_eq!(miners.len(), 2);
    assert_eq!(store.get_fee_pool()?, 8000);

    // miner_1: 1 bloc → 8000 * 1/3 = 2666
    // miner_2: 2 blocs → 8000 * 2/3 = 5333
    let total_blocks: u64 = miners.iter().map(|(_, c)| *c).sum();
    assert_eq!(total_blocks, 3);

    println!("✅ Multi-cycle test passed!");

    Ok(())
}

/// Test: Edge case - pool vide
#[tokio::test]
async fn test_empty_pool_distribution() -> Result<()> {
    let tr = test_rocks_store("node_rewards_empty").await?;
    let store = tr.store.clone();

    // Blocs minés mais pas de fees
    store.increment_node_block_count("miner_x")?;
    store.increment_node_block_count("miner_y")?;

    let pool = store.get_fee_pool()?;
    assert_eq!(pool, 0, "Pool should be empty");

    // Distribution avec pool vide = rien à distribuer
    let miners = store.get_all_miners()?;
    let total_blocks: u64 = miners.iter().map(|(_, c)| *c).sum();

    for (_pk, count) in &miners {
        let share = if total_blocks > 0 {
            pool * count / total_blocks
        } else {
            0
        };
        assert_eq!(share, 0, "Share should be 0 with empty pool");
    }

    Ok(())
}

/// Test: Edge case - aucun mineur
#[tokio::test]
async fn test_no_miners_distribution() -> Result<()> {
    let tr = test_rocks_store("node_rewards_no_miners").await?;
    let store = tr.store.clone();

    // Pool avec fees mais aucun mineur
    store.add_to_fee_pool(10000)?;

    let miners = store.get_all_miners()?;
    assert!(miners.is_empty(), "Should have no miners");

    // Dans ce cas, le pool reste intact jusqu'au prochain cycle
    assert_eq!(store.get_fee_pool()?, 10000);

    Ok(())
}
