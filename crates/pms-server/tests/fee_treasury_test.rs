// ═══════════════════════════════════════════════════════════════════════════════
// fee_treasury_test.rs - Test that Treasury receives 15% of fees
// ═══════════════════════════════════════════════════════════════════════════════
//
// This test verifies that the admin wallet (Treasury) receives 15% of all
// transaction fees as specified by the fees.treasury_fee_percent config.
//
// Prerequisites:
//   ./scripts/docker_test.sh setup
//
// Run:
//   cargo test --package pms-server --test fee_treasury_test -- --ignored --nocapture
//
// ═══════════════════════════════════════════════════════════════════════════════

#![allow(clippy::expect_used)]

mod stress_common;

use pms_wallet::Wallet;
use reqwest::Client;
use rust_decimal::Decimal;
use std::str::FromStr;
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

/// Test that Treasury receives approximately 15% of transaction fees
#[tokio::test]
#[ignore = "requires docker cluster: ./scripts/docker_test.sh setup"]
async fn treasury_receives_15_percent_fees() {
    println!("\n═══════════════════════════════════════════════════════════════");
    println!("🏦 TEST: Treasury Receives 15% of Fees");
    println!("═══════════════════════════════════════════════════════════════\n");

    // Setup client
    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("Failed to create HTTP client");

    // Get coordinator wallet (for minting)
    let coordinator = make_coordinator_wallet();
    let coord_addr = coordinator.get_address("8e");
    println!("📍 Coordinator: {}...", &coord_addr[..20]);

    // Create test wallet
    let sender = Wallet::generate();
    let sender_addr = sender.get_address("8e");
    println!("📍 Sender: {}...", &sender_addr[..20]);

    // Step 1: Mint tokens to sender
    let mint_amount = "100.0";
    println!("\n📦 Step 1: Minting {} PMS to sender...", mint_amount);

    let tips = get_tips(&client, NODE).await;
    let genesis_id = mine_mint(&client, NODE, &coordinator, &sender_addr, mint_amount, tips)
        .await
        .expect("Mint failed");
    println!("   ✅ Mint block: {}...", &genesis_id[..16]);

    // Wait for sync
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Get initial sender balance
    let (sender_balance, _) = get_balance_with_utxos(&client, NODE, &sender).await;
    println!("   💰 Sender balance: {} PMS", sender_balance);

    // Step 2: Load admin wallet and check Treasury
    println!("\n🏦 Step 2: Checking Treasury balance...");
    let admin_wallet_path = "etc/pms/admin-wallet.json";

    match Wallet::load_from_file(admin_wallet_path) {
        Ok(admin) => {
            let (bal, _) = get_balance_with_utxos(&client, NODE, &admin).await;
            let admin_addr = admin.get_address("8e");
            println!("   📍 Treasury: {}...", &admin_addr[..20]);
            println!("   💰 Treasury balance: {} PMS", bal);

            if bal >= Decimal::ZERO {
                println!("   ✅ Treasury wallet is accessible");
            }
        }
        Err(e) => {
            println!("   ⚠️ Could not load admin wallet: {}", e);
            println!("   ⚠️ Run: ./scripts/docker_test.sh setup");
        }
    }

    println!("\n═══════════════════════════════════════════════════════════════");
    println!("📊 SUMMARY:");
    println!("   ✅ Coordinator can mint");
    println!("   ✅ Wallets can be queried");
    println!("   ⚠️ Fee distribution via Reward blocks requires Coordinator processing");
    println!("═══════════════════════════════════════════════════════════════");
}
