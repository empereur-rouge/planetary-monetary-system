// ═══════════════════════════════════════════════════════════════════════════════
// total_supply_test.rs - Test Token Conservation Law
// ═══════════════════════════════════════════════════════════════════════════════
//
// This test verifies that total supply stays consistent after mint operations.
//
// Prerequisites:
//   ./scripts/docker_test.sh setup
//
// Run:
//   cargo test --package pms-server --test total_supply_test -- --ignored --nocapture
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

/// Test that total supply matches minted amount
#[tokio::test]
#[ignore = "requires docker cluster: ./scripts/docker_test.sh setup"]
async fn total_supply_conservation() {
    println!("\n═══════════════════════════════════════════════════════════════");
    println!("💰 TEST: Total Supply Conservation");
    println!("═══════════════════════════════════════════════════════════════\n");

    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("Failed to create HTTP client");

    // Setup wallets
    let coordinator = make_coordinator_wallet();
    let alice = Wallet::generate();
    let bob = Wallet::generate();

    let alice_addr = alice.get_address("8e");
    let bob_addr = bob.get_address("8e");

    println!("📍 Alice: {}...", &alice_addr[..20]);
    println!("📍 Bob: {}...", &bob_addr[..20]);

    // Step 1: Mint to Alice
    let mint_amount = Decimal::from(1000);
    println!("\n📦 Step 1: Minting {} PMS to Alice...", mint_amount);

    let tips = get_tips(&client, NODE).await;
    mine_mint(
        &client,
        NODE,
        &coordinator,
        &alice_addr,
        &mint_amount.to_string(),
        tips,
    )
    .await
    .expect("Mint failed");

    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Step 2: Mint to Bob
    let mint_amount_bob = Decimal::from(500);
    println!("📦 Step 2: Minting {} PMS to Bob...", mint_amount_bob);

    let tips = get_tips(&client, NODE).await;
    mine_mint(
        &client,
        NODE,
        &coordinator,
        &bob_addr,
        &mint_amount_bob.to_string(),
        tips,
    )
    .await
    .expect("Mint failed");

    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Step 3: Verify balances
    println!("\n🔍 Step 3: Checking Balances...");
    let (alice_bal, _) = get_balance_with_utxos(&client, NODE, &alice).await;
    let (bob_bal, _) = get_balance_with_utxos(&client, NODE, &bob).await;

    println!("   Alice: {} PMS", alice_bal);
    println!("   Bob: {} PMS", bob_bal);

    let total_minted = mint_amount + mint_amount_bob;
    let total_tracked = alice_bal + bob_bal;

    println!("\n═══════════════════════════════════════════════════════════════");
    println!("📊 RESULTS:");
    println!("   Total minted: {} PMS", total_minted);
    println!("   Total tracked: {} PMS", total_tracked);

    let diff = (total_minted - total_tracked).abs();
    println!("   Difference: {} PMS", diff);
    println!("═══════════════════════════════════════════════════════════════");

    if diff == Decimal::ZERO {
        println!("✅ PASS: Total supply is perfectly conserved");
    } else {
        println!("⚠️ WARNING: Supply difference of {} PMS detected", diff);
    }

    assert_eq!(total_minted, total_tracked, "Supply mismatch!");
}
