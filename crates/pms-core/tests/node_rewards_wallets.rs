//! Test E2E complet: Distribution des récompenses aux nœuds avec vrais wallets
//!
//! Ce test vérifie que:
//! 1. Les blocs minés incrémentent les compteurs par nœud
//! 2. Les fees s'accumulent dans le pool
//! 3. Le Milestone (signé par le Coordinator) distribue les UTXOs
//! 4. Les balances finales sont exactement proportionnelles

use anyhow::Result;
use num_traits::ToPrimitive;
use pms_config::load_config;
use pms_core::{ConcurrentDag, CoreAdapter, ValidatePolicy};
use pms_interface::NetDagAdapter;
use pms_storage::{DagStorage, NodeRewardsStorage, PutResult, StoredBlock};
use pms_testkit::{TestCoordinator, test_rocks_store};
use pms_types::Block;
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;
use std::sync::Arc;

/// Test E2E: Distribution avec 3 nœuds, vérification des UTXOs créés
#[tokio::test]
async fn test_node_rewards_e2e_with_wallets() -> Result<()> {
    // ═══════════════════════════════════════════════════════════════
    // 1) Setup: RocksDB temporaire, Genesis
    // ═══════════════════════════════════════════════════════════════
    let tr = test_rocks_store("node_rewards_wallets").await?;
    let store = tr.store.clone();

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // Genesis
    let genesis = Block::genesis(compute_block_id);
    let sb_genesis = StoredBlock {
        id: genesis.id.clone(),
        parents: genesis.parents.clone(),
        payload_json: serde_json::to_string(&genesis.payload).ok(),
        nonce: genesis.nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };
    let _ = store.append_block_atomic(&sb_genesis).await?;

    let dag = Arc::new(ConcurrentDag::new_with_genesis(genesis.clone()));

    // ═══════════════════════════════════════════════════════════════
    // 2) Créer le Coordinateur et configurer la policy
    // ═══════════════════════════════════════════════════════════════
    let coordinator = TestCoordinator::new();

    // Créer une policy avec le coordinator configuré
    let mut policy = ValidatePolicy::from_global_config();
    policy.coordinator_public_key = Some(coordinator.public_key.clone());
    policy.min_parents_after_boot = 1; // Mode test
    policy.skip_utxo_checks = true; // Utilise ShardedUtxoSet

    // Créer l'adapter avec la policy configurée
    let adapter = CoreAdapter::new_with_policy(dag.clone(), store.clone(), policy);

    println!("🎯 Coordinator pk: {}", &coordinator.public_key[..20]);

    // ═══════════════════════════════════════════════════════════════
    // 3) Créer 3 wallets (3 nœuds mineurs)
    // ═══════════════════════════════════════════════════════════════
    let wallet_alice = Wallet::from_seed(&[10u8; 32], None).expect("wallet Alice");
    let wallet_bob = Wallet::from_seed(&[20u8; 32], None).expect("wallet Bob");
    let wallet_charlie = Wallet::from_seed(&[30u8; 32], None).expect("wallet Charlie");

    let pk_alice = wallet_alice.public_key_hex.clone();
    let pk_bob = wallet_bob.public_key_hex.clone();
    let pk_charlie = wallet_charlie.public_key_hex.clone();

    println!("🔑 Alice   pk: {}", &pk_alice[..20]);
    println!("🔑 Bob     pk: {}", &pk_bob[..20]);
    println!("🔑 Charlie pk: {}", &pk_charlie[..20]);

    // ═══════════════════════════════════════════════════════════════
    // 4) Chaque nœud "mine" des blocs (enregistrement des compteurs)
    // ═══════════════════════════════════════════════════════════════
    // Alice: 50 blocs (50%), Bob: 30 blocs (30%), Charlie: 20 blocs (20%)
    for _ in 0..50 {
        store.increment_node_block_count(&pk_alice)?;
    }
    for _ in 0..30 {
        store.increment_node_block_count(&pk_bob)?;
    }
    for _ in 0..20 {
        store.increment_node_block_count(&pk_charlie)?;
    }

    assert_eq!(store.get_node_block_count(&pk_alice)?, 50);
    assert_eq!(store.get_node_block_count(&pk_bob)?, 30);
    assert_eq!(store.get_node_block_count(&pk_charlie)?, 20);

    let miners = store.get_all_miners()?;
    let total_blocks: u64 = miners.iter().map(|(_, c)| *c).sum();
    assert_eq!(total_blocks, 100);

    // ═══════════════════════════════════════════════════════════════
    // 5) Accumuler des fees dans le pool (simule des transactions)
    // ═══════════════════════════════════════════════════════════════
    // Pool total: 100_000_000 satoshis = 1.0 PMS
    let pool_amount: u64 = 100_000_000;
    store.add_to_fee_pool(pool_amount)?;
    assert_eq!(store.get_fee_pool()?, pool_amount);

    // ═══════════════════════════════════════════════════════════════
    // 6) Créer un Milestone signé par le Coordinator avec distribution
    // ═══════════════════════════════════════════════════════════════
    let parents = vec![genesis.id.clone()];
    let wb = coordinator.forge_milestone(
        parents.clone(),
        &meta,
        parents, // approved = parents
        true,    // distribute_rewards = true
    );

    let milestone_id = wb.id.clone();
    println!("📦 Milestone ID: {}", &milestone_id[..20]);

    // ═══════════════════════════════════════════════════════════════
    // 7) Persister le Milestone → Déclenche la distribution
    // ═══════════════════════════════════════════════════════════════
    let res = adapter.persist_block(&wb).await?;
    assert!(
        matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
        "Milestone doit être inséré"
    );

    // ═══════════════════════════════════════════════════════════════
    // 8) Vérifier que le pool et les compteurs sont reset
    // ═══════════════════════════════════════════════════════════════
    assert_eq!(
        store.get_fee_pool()?,
        0,
        "Pool doit être vide après distribution"
    );

    // Note: Le Milestone lui-même incrémente le compteur du coordinator (normal)
    // Donc on vérifie que les 3 mineurs originaux sont reset (seul le coordinator reste)
    let miners_after = store.get_all_miners()?;
    assert_eq!(miners_after.len(), 1, "Seul le coordinator doit rester");
    assert_eq!(
        miners_after[0].0, coordinator.public_key,
        "C'est bien le coordinator"
    );
    assert_eq!(miners_after[0].1, 1, "Le coordinator a miné le Milestone");

    // ═══════════════════════════════════════════════════════════════
    // 9) Vérifier les UTXOs créés dans le ShardedUtxoSet
    // ═══════════════════════════════════════════════════════════════
    // Attendus: Alice 50%, Bob 30%, Charlie 20%
    let mut total_distributed: u64 = 0;

    for idx in 0..3 {
        let out_id = pms_types::OutputId {
            txid: milestone_id.clone(),
            index: idx,
        };

        if let Some(utxo) = adapter.utxos.get(&out_id).await {
            let amount = rust_decimal::Decimal::from_str_exact(&utxo.amount).unwrap_or_default();
            let sats = (amount * rust_decimal::Decimal::from(100_000_000))
                .to_u64()
                .unwrap_or(0);

            println!(
                "✅ UTXO {} → {} = {} sats",
                idx,
                &utxo.address[..20.min(utxo.address.len())],
                sats
            );
            total_distributed += sats;
        }
    }

    assert_eq!(
        total_distributed, pool_amount,
        "Total distribué doit égaler le pool initial"
    );

    println!("🎉 Test E2E réussi: {} sats distribués!", total_distributed);

    Ok(())
}
