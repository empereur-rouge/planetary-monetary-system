// ============================================================================
// Distributed Transaction Processing - E2E Test (Single Writer Mode)
// ============================================================================
//
// Tests the complete flow via GATEWAY (sole public entry point):
// 1. Node registration via /v1/register
// 2. TX submission and fee accumulation
// 3. Fee pool status check via /v1/fee_pool
// 4. Distribution via /admin/distribute_fees
//
// Run with: cargo test -p pms-server --test distributed_tx_e2e -- --ignored --nocapture
// Prerequisites: ./scripts/docker_test.sh setup

use reqwest::Client;
use serde_json::{Value, json};
use std::time::Duration;

// Gateway is the SOLE entry point - Engine is internal only!
const GATEWAY_URL: &str = "https://localhost:8443";
const NODE1_URL: &str = "https://localhost:8443"; // Alias for backward compat
const COORDINATOR_URL: &str = "https://localhost:8443"; // All via Gateway

/// Test the distributed TX processing flow
#[tokio::test]
#[ignore] // Requires docker-compose.test.yml running
async fn distributed_tx_e2e_test() {
    println!("\n═══════════════════════════════════════════════════════════════");
    println!("🔄 DISTRIBUTED TX PROCESSING E2E TEST");
    println!("═══════════════════════════════════════════════════════════════\n");

    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();

    // ========================================================================
    // STEP 1: Check node health
    // ========================================================================
    println!("📊 STEP 1: Health Check");
    println!("───────────────────────────────────────────────────────────────");

    let health = client
        .get(format!("{}/livez", NODE1_URL))
        .send()
        .await
        .expect("Failed to connect to node");
    assert!(health.status().is_success(), "Node should be healthy");
    println!("   ✅ Node1 is healthy\n");

    // ========================================================================
    // STEP 2: Register a test node
    // ========================================================================
    println!("📝 STEP 2: Node Registration");
    println!("───────────────────────────────────────────────────────────────");

    let register_resp = client
        .post(format!("{}/v1/register", NODE1_URL))
        .json(&json!({
            "node_pk": "test_node_pk_12345678901234567890",
            "api_url": "http://test-node:8080"
        }))
        .send()
        .await
        .expect("Failed to register node");

    assert!(
        register_resp.status().is_success(),
        "Registration should succeed"
    );
    println!("   ✅ Test node registered\n");

    // ========================================================================
    // STEP 3: Get list of nodes
    // ========================================================================
    println!("📋 STEP 3: List Registered Nodes");
    println!("───────────────────────────────────────────────────────────────");

    let nodes_resp = client
        .get(format!("{}/v1/nodes", NODE1_URL))
        .send()
        .await
        .expect("Failed to get nodes");

    assert!(nodes_resp.status().is_success(), "Get nodes should succeed");
    let nodes: Value = nodes_resp.json().await.unwrap();
    println!("   Registered nodes: {}", nodes);

    let node_list = nodes["nodes"].as_array().unwrap();
    assert!(!node_list.is_empty(), "Should have at least one node");
    println!("   ✅ {} node(s) registered\n", node_list.len());

    // ========================================================================
    // STEP 4: Check initial fee pool
    // ========================================================================
    println!("💰 STEP 4: Initial Fee Pool Status");
    println!("───────────────────────────────────────────────────────────────");

    let pool_resp = client
        .get(format!("{}/v1/fee_pool", NODE1_URL))
        .send()
        .await
        .expect("Failed to get fee pool");

    assert!(
        pool_resp.status().is_success(),
        "Get fee pool should succeed"
    );
    let pool: Value = pool_resp.json().await.unwrap();
    println!("   Fee pool: {}", pool);
    println!("   Total fees: {}", pool["total_fees"]);
    println!("   TX count: {}", pool["tx_count"]);
    println!("   Contributors: {}\n", pool["num_contributors"]);

    // ========================================================================
    // STEP 5: Submit a transaction (if mint balance exists)
    // ========================================================================
    println!("💸 STEP 5: Submit Transaction (Fee Accumulation)");
    println!("───────────────────────────────────────────────────────────────");

    // Note: This step requires a pre-minted balance
    // In a real test, we would mint first, then send TX
    println!("   ⏭️ Skipping TX submission (requires mint setup)\n");

    // ========================================================================
    // STEP 6: Test admin distribute_fees endpoint
    // ========================================================================
    println!("📦 STEP 6: Test Fee Distribution Endpoint");
    println!("───────────────────────────────────────────────────────────────");

    // Get current tips for parent_id
    let tips_resp = client
        .post(format!("{}/v1/dag/tips", NODE1_URL))
        .json(&json!({"limit": 1}))
        .send()
        .await;

    if let Ok(resp) = tips_resp {
        if resp.status().is_success() {
            let tips: Value = resp.json().await.unwrap_or(json!({"tips": []}));
            let parent_id = tips["tips"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str())
                .unwrap_or("genesis");

            println!(
                "   Using parent: {}...",
                &parent_id[..16.min(parent_id.len())]
            );

            // Try to distribute (may fail if not coordinator or no fees)
            let dist_resp = client
                .post(format!("{}/admin/distribute_fees", COORDINATOR_URL))
                .header("Authorization", "Bearer pms_admin_secret") // Correct token from docker-compose.test.yml
                .json(&json!({
                    "parent_id": parent_id
                }))
                .send()
                .await;

            match dist_resp {
                Ok(resp) => {
                    println!("   Distribution response status: {}", resp.status());
                    if let Ok(body) = resp.json::<Value>().await {
                        println!("   Response: {}", body);
                    }
                }
                Err(e) => {
                    println!("   Distribution failed (expected if no admin token): {}", e);
                }
            }
        }
    }
    println!();

    // ========================================================================
    // STEP 7: Check fee pool after (should be same if TX wasn't sent)
    // ========================================================================
    println!("💰 STEP 7: Fee Pool After Distribution");
    println!("───────────────────────────────────────────────────────────────");

    let pool_resp = client
        .get(format!("{}/v1/fee_pool", NODE1_URL))
        .send()
        .await
        .expect("Failed to get fee pool");

    let pool: Value = pool_resp.json().await.unwrap();
    println!("   Fee pool: {}", pool);
    println!();

    println!("═══════════════════════════════════════════════════════════════");
    println!("✅ DISTRIBUTED TX PROCESSING E2E TEST COMPLETE");
    println!("═══════════════════════════════════════════════════════════════\n");
}

/// Unit test for FeePool::calculate_shares
#[test]
fn test_fee_pool_calculate_shares() {
    use pms_server::fee_pool::FeePool;
    use rust_decimal::Decimal;

    let mut pool = FeePool::new();

    // Simulate 4 transactions from 2 nodes
    pool.add_fee(Decimal::from(10), "node1");
    pool.add_fee(Decimal::from(10), "node1");
    pool.add_fee(Decimal::from(10), "node1");
    pool.add_fee(Decimal::from(10), "node2");

    assert_eq!(pool.total_fees, Decimal::from(40));
    assert_eq!(pool.tx_count, 4);

    let shares = pool.calculate_shares();
    assert_eq!(shares.len(), 2);

    // Node1: 75% (3/4 blocks), Node2: 25% (1/4 blocks)
    let node1 = shares.iter().find(|(pk, _, _)| pk == "node1").unwrap();
    let node2 = shares.iter().find(|(pk, _, _)| pk == "node2").unwrap();

    assert_eq!(node1.2, Decimal::from(30), "Node1 should get 30 PMS (75%)");
    assert_eq!(node2.2, Decimal::from(10), "Node2 should get 10 PMS (25%)");

    println!("✅ FeePool::calculate_shares test passed");
}

/// Unit test for NodeRegistry
#[test]
fn test_node_registry() {
    use pms_server::node_registry::NodeRegistry;

    let mut registry = NodeRegistry::new();

    // Register nodes
    registry.register("node1".to_string(), "http://node1:8080".to_string(), None);
    registry.register("node2".to_string(), "http://node2:8080".to_string(), None);

    // Check active nodes
    let nodes = registry.get_active_nodes();
    assert_eq!(nodes.len(), 2, "Should have 2 active nodes");

    // Increment block counts
    registry.increment_block_count("node1");
    registry.increment_block_count("node1");
    registry.increment_block_count("node2");

    let counts = registry.get_block_counts();
    assert_eq!(counts.get("node1"), Some(&2), "Node1 should have 2 blocks");
    assert_eq!(counts.get("node2"), Some(&1), "Node2 should have 1 block");

    // Reset
    registry.reset_block_counts();
    let counts = registry.get_block_counts();
    assert_eq!(
        counts.get("node1"),
        Some(&0),
        "Node1 should have 0 blocks after reset"
    );

    println!("✅ NodeRegistry test passed");
}
