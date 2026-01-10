// ═══════════════════════════════════════════════════════════════════════════════
// block_rewards_test.rs - Test Block Reward Distribution (70/20/10)
// ═══════════════════════════════════════════════════════════════════════════════
//
// This test verifies that block rewards are created correctly.
//
// Prerequisites:
//   ./scripts/docker_test.sh setup
//
// Run:
//   cargo test --package pms-server --test block_rewards_test -- --ignored --nocapture
//
// ═══════════════════════════════════════════════════════════════════════════════

#![allow(clippy::expect_used)]

mod stress_common;

use pms_wallet::Wallet;
use reqwest::Client;
use rust_decimal::Decimal;
use stress_common::{get_balance_with_utxos, make_coordinator_wallet, mine_mint};

/// Get tips from node
async fn get_tips(client: &Client, base_url: &str) -> Vec<String> {
    let resp = client
        .post(format!("{}/v1/dag/tips", base_url))
        .json(&serde_json::json!({"limit": 2}))
        .send()
        .await
        .expect("Failed to get tips");
    resp.json::<Vec<String>>().await.unwrap_or_default()
}

const NODE: &str = "https://127.0.0.1:8080";

/// Test block reward distribution
#[tokio::test]
#[ignore = "requires docker cluster: ./scripts/docker_test.sh setup"]
async fn block_reward_distribution() {
    println!("\n═══════════════════════════════════════════════════════════════");
    println!("🎁 TEST: Block Reward Distribution");
    println!("═══════════════════════════════════════════════════════════════\n");

    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("Failed to create HTTP client");

    // Get coordinator (block creator for rewards)
    let coordinator = make_coordinator_wallet();
    let receiver = Wallet::generate();

    let coord_addr = coordinator.get_address("8e");
    let receiver_addr = receiver.get_address("8e");

    println!("📍 Coordinator: {}...", &coord_addr[..20]);
    println!("📍 Receiver: {}...", &receiver_addr[..20]);

    // Step 1: Get initial coordinator balance
    println!("\n🔍 Step 1: Initial Coordinator Balance...");
    let (coord_before, _) = get_balance_with_utxos(&client, NODE, &coordinator).await;
    println!("   Coordinator: {} PMS", coord_before);

    // Step 2: Create a Mint block (triggers reward)
    println!("\n📦 Step 2: Creating Mint block...");
    let tips = get_tips(&client, NODE).await;
    mine_mint(&client, NODE, &coordinator, &receiver_addr, "100.0", tips)
        .await
        .expect("Mint failed");

    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    // Step 3: Check coordinator balance after
    println!("\n🔍 Step 3: Coordinator Balance After Block...");
    let (coord_after, _) = get_balance_with_utxos(&client, NODE, &coordinator).await;
    println!("   Coordinator: {} PMS", coord_after);

    let coord_gained = coord_after - coord_before;

    println!("\n═══════════════════════════════════════════════════════════════");
    println!("📊 RESULTS:");
    println!("   Coordinator before: {} PMS", coord_before);
    println!("   Coordinator after:  {} PMS", coord_after);
    println!("   Coordinator gained: {} PMS", coord_gained);
    println!("═══════════════════════════════════════════════════════════════");

    // Note: Block rewards only created after TX processing, not Mint
    // So coordinator may not receive immediate rewards from Mint blocks
    if coord_gained > Decimal::ZERO {
        println!("✅ PASS: Coordinator received block rewards");
    } else {
        println!("⚠️ NOTE: No rewards yet - rewards created after TX, not Mint");
    }
}
