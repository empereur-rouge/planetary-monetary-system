use anyhow::Result;
use pms_types::{Block, PayloadEnvelope};
use pms_utils::{compute_block_id, submit_block_http_to};
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::{WireBlock, WireMeta};
use reqwest::Client;
use std::env;
use std::sync::Arc;
use tokio::time::Instant;

#[tokio::test]
#[ignore]
async fn docker_load_test() -> Result<()> {
    // Configuration
    let base_url = env::var("PMS_API_URL").unwrap_or_else(|_| "https://127.0.0.1:8080".to_string());
    // Reduce load if running on weaker dev machine
    let concurrent_workers = 5;
    let blocks_per_worker = 10;
    let difficulty_bits = 16;

    println!("🚀 Starting Load Test on {}", base_url);
    println!(
        "Workers: {}, Blocks/Worker: {}, Total Expected: {}",
        concurrent_workers,
        blocks_per_worker,
        concurrent_workers * blocks_per_worker
    );

    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()?;
    let client = Arc::new(client);
    let base_url = Arc::new(base_url);

    // Initial Parent (Genesis or Tip)
    let genesis = Block::genesis(compute_block_id);
    // For simplicity in load test, everyone builds on Genesis or a fixed parent to encourage branching/DAG width.
    // In a real network, they would fetch tips. Here we want to stress ingestion.
    // Let's assume we can query tips once and fan out.

    // Fetch generic parents once
    let parents = vec![genesis.id.clone()];
    let parents = Arc::new(parents);

    let start = Instant::now();
    let mut handles = Vec::new();

    for i in 0..concurrent_workers {
        let client = client.clone();
        let base_url = base_url.clone();
        let parents = parents.clone();

        // Each worker has its own wallet to simulate different nodes
        let wallet = Arc::new(Wallet::generate());

        handles.push(tokio::spawn(async move {
            let mut success = 0;
            let mut errors = 0;

            for _j in 0..blocks_per_worker {
                // Unique payload to ensure unique ID even if nonce collides (unlikely with random wallets but safe)
                // Actually payload is None often. Nonce variance is enough.
                let payload: Option<PayloadEnvelope> = None;

                // Mining
                // Use huge random space to avoid collision
                let mut nonce: u64 = rand::random();
                let mut block =
                    Block::new(parents.as_ref().clone(), payload, nonce, compute_block_id)
                        .expect("block creation");

                loop {
                    if block.id.starts_with("0000") {
                        // 16 bits approx check
                        break;
                    }
                    block.nonce += 1;
                    block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
                }

                // Signing
                let mut wb = WireBlock {
                    id: block.id.clone(),
                    parents: block.parents.clone(),
                    payload_json: serde_json::to_string(&block.payload).ok(),
                    nonce: block.nonce,
                    network_id: "pms-test".to_string(),
                    protocol_version: 1,
                    signer_pk_hex: wallet.encoded_public_key(),
                    signature_hex: String::new(),
                };
                let msg = canonical_wireblock_message(&wb);
                wb.signature_hex = wallet.sign(&msg).unwrap();

                // Submit
                let resp = client
                    .post(format!("{}/submit/block", base_url))
                    .json(&wb)
                    .send()
                    .await;

                match resp {
                    Ok(r) if r.status().is_success() => success += 1,
                    Ok(r) => {
                        let status = r.status();
                        let text = r.text().await.unwrap_or_default();
                        println!("❌ Worker {} failed block {}: {} - {}", i, _j, status, text);
                        errors += 1;
                    }
                    Err(e) => {
                        println!("❌ Worker {} connection error: {}", i, e);
                        errors += 1;
                    }
                }
            }
            (success, errors)
        }));
    }

    let mut total_success = 0;
    let mut total_errors = 0;

    for h in handles {
        let (s, e) = h.await?;
        total_success += s;
        total_errors += e;
    }

    let duration = start.elapsed();
    println!("🏁 Load Test Finished in {:.2?}", duration);
    println!("✅ Success: {}", total_success);
    println!("❌ Errors:  {}", total_errors);

    let tps = total_success as f64 / duration.as_secs_f64();
    println!("⚡ Throughput: {:.2} blocks/sec", tps);

    // Assertions
    // We expect high success rate.
    // If we spam genesis children, we might hit "max parents" or verify rules if we were fetching tips.
    // Given we are static parents, we just create a extremely wide DAG (star shape).
    assert!(
        total_success > (concurrent_workers * blocks_per_worker) * 9 / 10,
        "Success rate < 90%"
    );

    Ok(())
}
