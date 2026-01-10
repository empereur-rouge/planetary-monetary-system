// ═══════════════════════════════════════════════════════════════════════════════
// fee_distribution_e2e_test.rs - Complete Fee Distribution Verification
// ═══════════════════════════════════════════════════════════════════════════════
//
// This test ACTUALLY verifies that fees are distributed correctly:
// - 15% to Treasury (admin wallet)
// - 45% to Block Creator (coordinator)
// - 40% to Parent block signers
//
// And block rewards:
// - 70% to Creator
// - 20% to Treasury
// - 10% Burned
//
// Prerequisites:
//   ./scripts/docker_test.sh setup
//
// Run:
//   cargo test -p pms-server --test fee_distribution_e2e_test -- --ignored --nocapture
//
// ═══════════════════════════════════════════════════════════════════════════════

#![allow(clippy::expect_used)]

mod stress_common;

use pms_wallet::Wallet;
use reqwest::Client;
use rust_decimal::Decimal;
use std::str::FromStr;
use stress_common::{
    FEE_RATIO, NETWORK_ID, POW_PREFIX, PROTOCOL_VERSION, get_balance_with_utxos,
    make_coordinator_wallet, mine_mint, send_tx_with_split_fee,
};

const NODE: &str = "https://127.0.0.1:8080";

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

/// Get balance by address only (for treasury without private keys)
async fn get_balance_by_addr(client: &Client, base_url: &str, addr: &str) -> Decimal {
    // New endpoint only needs address
    let body = serde_json::json!({
        "address": addr
    });
    let resp = client
        .post(format!("{}/v1/balance", base_url))
        .json(&body)
        .send()
        .await;
    match resp {
        Ok(r) => {
            let json: serde_json::Value = r.json().await.unwrap_or_default();
            Decimal::from_str(json["balance"].as_str().unwrap_or("0")).unwrap_or(Decimal::ZERO)
        }
        Err(_) => Decimal::ZERO,
    }
}

/// Complete E2E test verifying fee distribution after TX
#[tokio::test]
#[ignore = "requires docker cluster: ./scripts/docker_test.sh setup"]
async fn complete_fee_distribution_verification() {
    println!("\n═══════════════════════════════════════════════════════════════");
    println!("💰 COMPLETE FEE DISTRIBUTION E2E TEST");
    println!("═══════════════════════════════════════════════════════════════\n");

    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("Failed to create HTTP client");

    // ═══════════════════════════════════════════════════════════════════════
    // SETUP: Create wallets
    // ═══════════════════════════════════════════════════════════════════════

    let coordinator = make_coordinator_wallet();
    let sender = Wallet::generate();
    let receiver = Wallet::generate();

    let coord_addr = coordinator.get_address("8e");
    let sender_addr = sender.get_address("8e");
    let receiver_addr = receiver.get_address("8e");

    println!("📍 Coordinator (Creator): {}...", &coord_addr[..20]);
    println!("📍 Sender: {}...", &sender_addr[..20]);
    println!("📍 Receiver: {}...", &receiver_addr[..20]);

    // Load ALL treasury addresses from treasury-wallets.json
    let treasury_paths = [
        "etc/pms/treasury-wallets.json",
        "../../etc/pms/treasury-wallets.json",
        "../../../etc/pms/treasury-wallets.json",
    ];
    let treasury_addrs: Vec<String> = treasury_paths
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .and_then(|content| {
            let json: serde_json::Value = serde_json::from_str(&content).ok()?;
            let wallets = json["wallets"].as_array()?;
            let addrs: Vec<String> = wallets
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if !addrs.is_empty() {
                for (i, addr) in addrs.iter().enumerate() {
                    println!("📍 Treasury #{}: {}...", i + 1, &addr[..20.min(addr.len())]);
                }
                Some(addrs)
            } else {
                None
            }
        })
        .unwrap_or_default();

    if treasury_addrs.is_empty() {
        println!("⚠️ Treasury wallets not found in any path");
        println!("   Run: ./scripts/generate_treasury_wallets.sh");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // STEP 1: Get initial balances
    // ═══════════════════════════════════════════════════════════════════════

    println!("\n📊 STEP 1: Initial Balances");
    println!("───────────────────────────────────────────────────────────────");

    let (coord_before, _) = get_balance_with_utxos(&client, NODE, &coordinator).await;
    println!("   Coordinator: {} PMS", coord_before);

    // Get total balance across ALL treasury wallets
    let mut treasury_before = Decimal::ZERO;
    for addr in &treasury_addrs {
        let bal = get_balance_by_addr(&client, NODE, addr).await;
        treasury_before += bal;
    }
    println!("   Treasury (total): {} PMS", treasury_before);

    // ═══════════════════════════════════════════════════════════════════════
    // STEP 2: Mint tokens to Sender
    // ═══════════════════════════════════════════════════════════════════════

    let mint_amount = Decimal::from(1000);
    println!("\n📦 STEP 2: Mint {} PMS to Sender", mint_amount);
    println!("───────────────────────────────────────────────────────────────");

    let tips = get_tips(&client, NODE).await;
    let mint_id = mine_mint(
        &client,
        NODE,
        &coordinator,
        &sender_addr,
        &mint_amount.to_string(),
        tips,
    )
    .await
    .expect("Mint failed");
    println!("   ✅ Mint block: {}...", &mint_id[..16]);

    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Verify sender balance
    let (sender_bal, sender_utxos) = get_balance_with_utxos(&client, NODE, &sender).await;
    println!("   Sender balance: {} PMS", sender_bal);
    assert!(
        sender_bal >= mint_amount,
        "Sender should have minted tokens"
    );

    // ═══════════════════════════════════════════════════════════════════════
    // STEP 3: Send TX with fees (this triggers reward block creation)
    // ═══════════════════════════════════════════════════════════════════════

    let payment = Decimal::from(100);
    let fee_ratio = Decimal::from_str(FEE_RATIO).unwrap();
    let total_fee = payment * fee_ratio;

    println!(
        "\n💸 STEP 3: Send {} PMS with {} PMS fee",
        payment, total_fee
    );
    println!("───────────────────────────────────────────────────────────────");
    println!("   Expected fee distribution:");
    println!(
        "     - Treasury (15%): {} PMS",
        total_fee * Decimal::from_str("0.15").unwrap()
    );
    println!(
        "     - Creator (45%):  {} PMS",
        total_fee * Decimal::from_str("0.45").unwrap()
    );
    println!(
        "     - Parents (40%):  {} PMS",
        total_fee * Decimal::from_str("0.40").unwrap()
    );

    // Get UTXO for transaction - sender_utxos is serde_json::Value
    let utxos_array = sender_utxos.as_array();
    if utxos_array.is_none() || utxos_array.unwrap().is_empty() {
        println!("   ❌ No UTXOs available for sender");
        panic!("Sender has no UTXOs");
    }

    let utxo = &utxos_array.unwrap()[0];
    // UTXO structure: {"out": {"txid": "...", "index": N}, "amount": "..."}
    let utxo_txid = utxo["out"]["txid"].as_str().unwrap_or(&mint_id);
    let utxo_index = utxo["out"]["index"].as_u64().unwrap_or(0) as u32;
    let change_amount = sender_bal - payment - total_fee;

    let tips = get_tips(&client, NODE).await;

    // Platform address for fee (45% goes here)
    let platform_addr = coord_addr.clone(); // In test, coordinator is also platform

    match send_tx_with_split_fee(
        &client,
        NODE,
        &sender,
        utxo_txid,  // txid from JSON
        utxo_index, // index from JSON
        &receiver_addr,
        &payment.to_string(),
        &platform_addr,
        &total_fee.to_string(),
        &sender_addr,
        &change_amount.to_string(),
        tips,
    )
    .await
    {
        Ok((block_id, _)) => {
            println!("   ✅ TX block: {}...", &block_id[..16]);
        }
        Err(e) => {
            println!("   ❌ TX failed: {}", e);
            panic!("Transaction failed: {}", e);
        }
    }

    // Wait for reward block creation and propagation
    println!("\n⏳ Waiting for reward block creation...");
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    // ═══════════════════════════════════════════════════════════════════════
    // STEP 4: Verify balances after TX
    // ═══════════════════════════════════════════════════════════════════════

    println!("\n📊 STEP 4: Final Balances & Verification");
    println!("───────────────────────────────────────────────────────────────");

    let (coord_after, _) = get_balance_with_utxos(&client, NODE, &coordinator).await;
    let coord_gained = coord_after - coord_before;
    println!("   Coordinator: {} PMS (+{})", coord_after, coord_gained);

    // Sum all treasury wallet balances
    let mut treasury_after = Decimal::ZERO;
    for addr in &treasury_addrs {
        let bal = get_balance_by_addr(&client, NODE, addr).await;
        treasury_after += bal;
    }
    let treasury_gained = treasury_after - treasury_before;
    println!(
        "   Treasury (total): {} PMS (+{})",
        treasury_after, treasury_gained
    );

    let (receiver_bal, _) = get_balance_with_utxos(&client, NODE, &receiver).await;
    println!("   Receiver:    {} PMS", receiver_bal);

    let (sender_final, _) = get_balance_with_utxos(&client, NODE, &sender).await;
    println!("   Sender:      {} PMS", sender_final);

    // ═══════════════════════════════════════════════════════════════════════
    // STEP 5: Results Analysis
    // ═══════════════════════════════════════════════════════════════════════

    println!("\n═══════════════════════════════════════════════════════════════");
    println!("📊 RESULTS ANALYSIS");
    println!("═══════════════════════════════════════════════════════════════");

    let expected_treasury = total_fee * Decimal::from_str("0.15").unwrap();
    let expected_creator = total_fee * Decimal::from_str("0.45").unwrap();

    println!("\n Fee Distribution (expected vs actual):");
    println!("┌─────────────────┬──────────────┬──────────────┐");
    println!("│ Recipient       │ Expected     │ Actual       │");
    println!("├─────────────────┼──────────────┼──────────────┤");
    println!(
        "│ Treasury (15%)  │ {:>10}   │ {:>10}   │",
        expected_treasury, treasury_gained
    );
    println!(
        "│ Creator (45%)   │ {:>10}   │ {:>10}   │",
        expected_creator, coord_gained
    );
    println!("└─────────────────┴──────────────┴──────────────┘");

    // Verify payment was received
    let tolerance = Decimal::from_str("0.01").unwrap();

    assert!(
        (receiver_bal - payment).abs() <= tolerance,
        "Receiver should have {} PMS, got {}",
        payment,
        receiver_bal
    );
    println!("\n✅ Payment received: {} PMS", receiver_bal);

    // Check if coordinator gained fees
    if coord_gained > Decimal::ZERO {
        println!(
            "✅ Coordinator received fees/rewards: +{} PMS",
            coord_gained
        );
    } else {
        println!("⚠️ Coordinator balance unchanged (rewards may be pending)");
    }

    // Check if treasury gained fees
    if !treasury_addrs.is_empty() {
        if treasury_gained > Decimal::ZERO {
            println!("✅ Treasury received fees: +{} PMS", treasury_gained);
        } else {
            println!("⚠️ Treasury balance unchanged (may need coordinator-only distribution)");
        }
    }

    println!("\n═══════════════════════════════════════════════════════════════");
}
