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

/// Test: les PRIMITIVES de stockage qui alimentent la distribution proportionnelle.
///
/// NB (v0.9.3) : l'ancienne version recalculait `pool * count / total` DANS le test
/// puis assertait `node_a_share == 6000` — soit `f(x) == f(x)`, une tautologie qui
/// ne testait pas le code de prod (la vraie distribution vit dans
/// `pms-server/src/fee_distribution/{compute,distribute}.rs` et est testée par
/// `pms-server/src/fee_distribution/tests.rs`). Ici on vérifie uniquement ce que
/// CETTE couche fournit : l'accumulation du pool, les compteurs par nœud, et leur
/// somme — les entrées exactes que le distributeur consomme.
#[tokio::test]
async fn test_fee_pool_and_miner_counts_feed_distribution() -> Result<()> {
    let tr = test_rocks_store("node_rewards_dist").await?;
    let store = tr.store.clone();

    // Pool: 10000 sats ; Node A: 6 blocs ; Node B: 4 blocs.
    store.add_to_fee_pool(4000)?;
    store.add_to_fee_pool(6000)?; // accumulation incrémentale → doit sommer à 10000
    for _ in 0..6 {
        store.increment_node_block_count("node_a")?;
    }
    for _ in 0..4 {
        store.increment_node_block_count("node_b")?;
    }

    let pool = store.get_fee_pool()?;
    let miners = store.get_all_miners()?;
    let total_blocks: u64 = miners.iter().map(|(_, c)| *c).sum();
    println!("pool={pool} miners={miners:?} total_blocks={total_blocks}");

    // Le pool accumule correctement (4000 + 6000).
    assert_eq!(pool, 10000, "fee pool must accumulate added amounts");
    // Les compteurs par nœud sont exacts et persistés.
    assert_eq!(store.get_node_block_count("node_a")?, 6);
    assert_eq!(store.get_node_block_count("node_b")?, 4);
    assert_eq!(total_blocks, 10, "sum of miner counts");
    assert_eq!(miners.len(), 2, "exactly two distinct miners tracked");

    Ok(())
}

/// Test: RuntimeConfig avec treasury_fee_bps
#[tokio::test]
async fn test_runtime_config_treasury_fee() -> Result<()> {
    let tr = test_rocks_store("node_rewards_config").await?;
    let store = tr.store.clone();

    // Config par défaut (Coordinator 67%, Treasury 33%)
    let config = store.get_runtime_config()?;
    assert_eq!(config.coordinator_fee_bps, 6700); // 67%
    assert_eq!(config.treasury_fee_bps, 3300); // 33%

    // Modifier via ConfigUpdate
    use pms_config::{ConfigUpdate, RuntimeConfig};

    let update = ConfigUpdate::BatchUpdate(vec![
        ConfigUpdate::SetTreasuryFee { bps: 4000 },
        ConfigUpdate::SetCoordinatorFee { bps: 6000 },
    ]);
    let new_config = config.apply_update(&update, "test-block", 12345).unwrap();

    assert_eq!(new_config.treasury_fee_bps, 4000);
    assert_eq!(new_config.coordinator_fee_bps, 6000);
    assert_eq!(new_config.updated_at_block, "test-block");

    Ok(())
}
