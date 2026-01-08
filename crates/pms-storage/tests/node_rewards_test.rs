//! Tests pour le système de distribution des fees aux nœuds.
//!
//! Ces tests vérifient :
//! - Le comptage des blocs par nœud
//! - L'accumulation du pool de fees
//! - La distribution proportionnelle via Milestone

use anyhow::Result;
use pms_storage::{ConfigStorage, NodeRewardsStorage};
use pms_testkit::test_rocks_store;

/// Test: Incrémentation des compteurs de blocs par nœud
#[tokio::test]
async fn test_node_block_count_increment() -> Result<()> {
    let tr = test_rocks_store("node_rewards_count").await?;
    let store = tr.store.clone();

    // Initial: 0 blocs
    assert_eq!(store.get_node_block_count("node_a")?, 0);
    assert_eq!(store.get_node_block_count("node_b")?, 0);

    // Incrémenter node_a 3 fois
    store.increment_node_block_count("node_a")?;
    store.increment_node_block_count("node_a")?;
    store.increment_node_block_count("node_a")?;

    // Incrémenter node_b 2 fois
    store.increment_node_block_count("node_b")?;
    store.increment_node_block_count("node_b")?;

    // Vérifier les compteurs
    assert_eq!(store.get_node_block_count("node_a")?, 3);
    assert_eq!(store.get_node_block_count("node_b")?, 2);

    Ok(())
}

/// Test: Accumulation du pool de fees
#[tokio::test]
async fn test_fee_pool_accumulation() -> Result<()> {
    let tr = test_rocks_store("node_rewards_pool").await?;
    let store = tr.store.clone();

    // Initial: pool vide
    assert_eq!(store.get_fee_pool()?, 0);

    // Ajouter au pool
    store.add_to_fee_pool(1000)?;
    assert_eq!(store.get_fee_pool()?, 1000);

    store.add_to_fee_pool(500)?;
    assert_eq!(store.get_fee_pool()?, 1500);

    store.add_to_fee_pool(2500)?;
    assert_eq!(store.get_fee_pool()?, 4000);

    Ok(())
}

/// Test: Récupération de tous les mineurs
#[tokio::test]
async fn test_get_all_miners() -> Result<()> {
    let tr = test_rocks_store("node_rewards_miners").await?;
    let store = tr.store.clone();

    // Ajouter des blocs pour différents nœuds
    for _ in 0..5 {
        store.increment_node_block_count("alice_pk")?;
    }
    for _ in 0..3 {
        store.increment_node_block_count("bob_pk")?;
    }
    for _ in 0..2 {
        store.increment_node_block_count("charlie_pk")?;
    }

    // Récupérer tous les mineurs
    let miners = store.get_all_miners()?;
    assert_eq!(miners.len(), 3);

    // Vérifier le total
    let total: u64 = miners.iter().map(|(_, c)| c).sum();
    assert_eq!(total, 10);

    // Trouver chaque mineur
    let alice = miners.iter().find(|(pk, _)| pk == "alice_pk").unwrap();
    let bob = miners.iter().find(|(pk, _)| pk == "bob_pk").unwrap();
    let charlie = miners.iter().find(|(pk, _)| pk == "charlie_pk").unwrap();

    assert_eq!(alice.1, 5);
    assert_eq!(bob.1, 3);
    assert_eq!(charlie.1, 2);

    Ok(())
}

/// Test: Reset du pool et des compteurs
#[tokio::test]
async fn test_reset_pool_and_counts() -> Result<()> {
    let tr = test_rocks_store("node_rewards_reset").await?;
    let store = tr.store.clone();

    // Setup: ajouter des données
    store.add_to_fee_pool(10000)?;
    store.increment_node_block_count("node_x")?;
    store.increment_node_block_count("node_x")?;
    store.increment_node_block_count("node_y")?;

    // Vérifier avant reset
    assert_eq!(store.get_fee_pool()?, 10000);
    assert_eq!(store.get_node_block_count("node_x")?, 2);
    assert_eq!(store.get_node_block_count("node_y")?, 1);
    assert_eq!(store.get_all_miners()?.len(), 2);

    // Reset
    store.reset_pool_and_counts()?;

    // Vérifier après reset
    assert_eq!(store.get_fee_pool()?, 0);
    assert_eq!(store.get_node_block_count("node_x")?, 0);
    assert_eq!(store.get_node_block_count("node_y")?, 0);
    assert_eq!(store.get_all_miners()?.len(), 0);

    Ok(())
}

/// Test: Calcul de distribution proportionnelle
#[tokio::test]
async fn test_distribution_calculation() -> Result<()> {
    let tr = test_rocks_store("node_rewards_dist").await?;
    let store = tr.store.clone();

    // Setup:
    // - Pool: 10000 sats
    // - Node A: 6 blocs (60%)
    // - Node B: 4 blocs (40%)
    store.add_to_fee_pool(10000)?;
    for _ in 0..6 {
        store.increment_node_block_count("node_a")?;
    }
    for _ in 0..4 {
        store.increment_node_block_count("node_b")?;
    }

    let pool = store.get_fee_pool()?;
    let miners = store.get_all_miners()?;
    let total_blocks: u64 = miners.iter().map(|(_, c)| *c).sum();

    assert_eq!(pool, 10000);
    assert_eq!(total_blocks, 10);

    // Calculer les parts
    let mut distributions = Vec::new();
    for (pk, count) in &miners {
        let share = pool * count / total_blocks;
        distributions.push((pk.clone(), share));
    }

    // Vérifier les parts
    let node_a_share = distributions
        .iter()
        .find(|(pk, _)| pk == "node_a")
        .unwrap()
        .1;
    let node_b_share = distributions
        .iter()
        .find(|(pk, _)| pk == "node_b")
        .unwrap()
        .1;

    assert_eq!(node_a_share, 6000); // 60% de 10000
    assert_eq!(node_b_share, 4000); // 40% de 10000

    Ok(())
}

/// Test: RuntimeConfig avec node_fee_bps
#[tokio::test]
async fn test_runtime_config_node_fee() -> Result<()> {
    let tr = test_rocks_store("node_rewards_config").await?;
    let store = tr.store.clone();

    // Config par défaut
    let config = store.get_runtime_config()?;
    assert_eq!(config.node_fee_bps, 3000); // 30%

    // Modifier via ConfigUpdate
    use pms_config::{ConfigUpdate, RuntimeConfig};

    let update = ConfigUpdate::SetNodeFee { bps: 2500 };
    let new_config = config.apply_update(&update, "test-block", 12345);

    assert_eq!(new_config.node_fee_bps, 2500);
    assert_eq!(new_config.updated_at_block, "test-block");

    Ok(())
}
