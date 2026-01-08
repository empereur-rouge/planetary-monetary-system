use anyhow::{Context, Result};
use pms_types::{Block, PayloadEnvelope};
use pms_utils::{compute_block_id, submit_block_http_to};
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::{WireBlock, WireMeta};
use reqwest::Client;
use std::env;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
#[ignore]
async fn e2e_scenario_1_minting_balance() -> Result<()> {
    // This test runs against a running Docker container.
    // Ensure you have run: ./setup_docker_node.sh (or docker compose up -d)

    // 1. Setup Client
    let base_url = env::var("PMS_API_URL").unwrap_or_else(|_| "https://127.0.0.1:8080".to_string());
    println!(
        "🚀 E2E Scenario 1: Minting & Balance targeting: {}",
        base_url
    );

    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()?;

    // Wait for health
    let mut online = false;
    for _ in 0..10 {
        if client
            .get(format!("{}/livez", base_url))
            .send()
            .await
            .is_ok()
        {
            online = true;
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    if !online {
        panic!("Node not reachable at {}", base_url);
    }

    // 2. Generate Wallet
    let wallet = Wallet::generate();
    let wallet_addr = wallet.get_address("8e");
    println!("👤 User Wallet: {}", wallet_addr);

    // 3. Mine 3 Blocks
    let mut parents = vec![];
    // Simple parent selection: get tips from stream or use genesis
    let tips_url = format!("{}/blocks/stream?limit=1", base_url);
    if let Ok(resp) = client.get(&tips_url).send().await {
        if let Ok(blocks) = resp.json::<Vec<WireBlock>>().await {
            if let Some(b) = blocks.last() {
                parents.push(b.id.clone());
            }
        }
    }
    if parents.is_empty() {
        // Assume genesis
        parents.push(Block::genesis(compute_block_id).id);
    }

    let blocks_to_mine = 3;
    println!("⛏️  Mining {} blocks...", blocks_to_mine);

    use pms_types::{PlainPayload, TxOutput};

    for i in 0..blocks_to_mine {
        println!("   Mining block {}/{}...", i + 1, blocks_to_mine);

        let output = TxOutput {
            address: wallet_addr.clone(),
            amount: "50.0".to_string(),
        };
        let payload = Some(PayloadEnvelope::Plain(PlainPayload::Mint {
            outputs: vec![output],
        }));

        let mut block = Block::new(parents.clone(), payload, 0, None, compute_block_id).unwrap();

        // Mine
        loop {
            if block.id.starts_with("0000") {
                break;
            }
            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
        }

        // Construct WireBlock
        let mut wb = WireBlock {
            id: block.id.clone(),
            parents: block.parents.clone(),
            payload_json: Some(
                serde_json::to_string(&block.payload).expect("Payload serialization failed"),
            ),
            nonce: block.nonce,
            network_id: "pms-test".to_string(),
            protocol_version: 1,
            signer_pk_hex: wallet.encoded_public_key(),
            signature_hex: String::new(),
            metadata: None,
        };

        // Sign
        let msg = canonical_wireblock_message(&wb);
        wb.signature_hex = wallet.sign(&msg).unwrap();

        // Submit
        let submit_url = format!("{}/submit/block", base_url);
        let resp = client.post(&submit_url).json(&wb).send().await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("Failed to read body: {}", e));
            panic!("Block submission failed: {}", err_text);
        }

        parents = vec![wb.id];
        sleep(Duration::from_millis(100)).await;
    }

    sleep(Duration::from_secs(2)).await; // Wait for async writes/indexing

    // 4. Verify Balance
    let balance_url = format!("{}/wallet/balance", base_url);

    let body = serde_json::json!({
        "bech32_addr": wallet_addr,
        "x25519_sk_hex": wallet.x25519_sk_hex().unwrap(),
        "ecdsa_pk_hex": wallet.encoded_public_key(),
        "scan_limit": 2000
    });

    let resp = client.post(&balance_url).json(&body).send().await?;

    let status = resp.status();
    let text = resp.text().await?;
    assert!(status.is_success(), "Balance request failed: {}", text);

    let json: serde_json::Value =
        serde_json::from_str(&text).context("Failed to parse balance JSON")?;
    let balance_str = json["balance"].as_str().unwrap_or("0");
    let balance: f64 = balance_str.parse().unwrap_or(0.0);

    println!("💰 Parsed Balance: {}", balance);
    assert_eq!(balance, 150.0);

    Ok(())
}

#[tokio::test]
#[ignore]
async fn e2e_scenario_2_transaction_fees() -> Result<()> {
    use pms_types::{OutputId, PlainPayload, Transaction, TxInput, TxOutput, Unlock};
    use rust_decimal::Decimal;
    use std::str::FromStr;

    let base_url = env::var("PMS_API_URL").unwrap_or_else(|_| "https://127.0.0.1:8080".to_string());
    println!("🚀 E2E Scenario 2 (Enhanced): Complex Transaction Network");
    println!("   Target: {}", base_url);

    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()?;

    // === WALLETS ===
    let alice = Wallet::generate();
    let bob = Wallet::generate();
    let carol = Wallet::generate();
    let dave = Wallet::generate();

    // Read real admin wallet from Docker container
    let (admin, admin_source) = {
        use std::process::Command;
        let output = Command::new("docker")
            .args([
                "compose",
                "exec",
                "-T",
                "node",
                "cat",
                "/home/pms/config/admin-wallet.json",
            ])
            .current_dir("/Users/erwan.ngma/Documents/Programations/Rust/dag-pms")
            .output();

        match output {
            Ok(out) if !out.stdout.is_empty() => {
                let json_str = String::from_utf8_lossy(&out.stdout);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&json_str) {
                    // Reconstruct Wallet from JSON fields
                    if let (Some(priv_key), Some(pub_key), Some(x25519_pub)) = (
                        json["private_key_b64"].as_str(),
                        json["public_key_hex"].as_str(),
                        json["x25519_pub_hex"].as_str(),
                    ) {
                        let wallet = Wallet {
                            private_key_b64: priv_key.to_string(),
                            public_key_hex: pub_key.to_string(),
                            x25519_pub_hex: x25519_pub.to_string(),
                            mnemonic_words: None, // Not needed for balance check
                        };
                        (wallet, "Docker")
                    } else {
                        (Wallet::generate(), "Generated (missing fields)")
                    }
                } else {
                    (Wallet::generate(), "Generated (parse error)")
                }
            }
            _ => (Wallet::generate(), "Generated (docker fail)"),
        }
    };

    let alice_addr = alice.get_address("8e");
    let bob_addr = bob.get_address("8e");
    let carol_addr = carol.get_address("8e");
    let dave_addr = dave.get_address("8e");
    let admin_addr = admin.get_address("8e");

    println!("👤 Alice: {}", &alice_addr[..20]);
    println!("👤 Bob:   {}", &bob_addr[..20]);
    println!("👤 Carol: {}", &carol_addr[..20]);
    println!("👤 Dave:  {}", &dave_addr[..20]);
    println!("💼 Admin ({}): {}", admin_source, &admin_addr[..20]);

    // === HELPER: Get current tips ===
    async fn get_tips(client: &Client, base_url: &str) -> Vec<String> {
        let tips_url = format!("{}/blocks/stream?limit=1", base_url);
        if let Ok(resp) = client.get(&tips_url).send().await {
            if let Ok(blocks) = resp.json::<Vec<WireBlock>>().await {
                if let Some(b) = blocks.last() {
                    return vec![b.id.clone()];
                }
            }
        }
        vec![Block::genesis(compute_block_id).id]
    }

    // === HELPER: Mine block with Mint ===
    async fn mine_mint(
        client: &Client,
        base_url: &str,
        miner: &Wallet,
        to_addr: &str,
        amount: &str,
        parents: Vec<String>,
    ) -> Result<String> {
        let output = TxOutput {
            address: to_addr.to_string(),
            amount: amount.to_string(),
        };
        let payload = Some(PayloadEnvelope::Plain(PlainPayload::Mint {
            outputs: vec![output],
        }));

        let mut block = Block::new(parents, payload, 0, None, compute_block_id).unwrap();
        loop {
            if block.id.starts_with("0000") {
                break;
            }
            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
        }

        let mut wb = WireBlock {
            id: block.id.clone(),
            parents: block.parents.clone(),
            payload_json: Some(serde_json::to_string(&block.payload)?),
            nonce: block.nonce,
            network_id: "pms-test".to_string(),
            protocol_version: 1,
            signer_pk_hex: miner.encoded_public_key(),
            signature_hex: String::new(),
            metadata: None,
        };
        wb.signature_hex = miner.sign(&canonical_wireblock_message(&wb)).unwrap();

        client
            .post(format!("{}/submit/block", base_url))
            .json(&wb)
            .send()
            .await?
            .error_for_status()?;

        Ok(wb.id)
    }

    // === HELPER: Get balance ===
    async fn get_balance(client: &Client, base_url: &str, wallet: &Wallet) -> Decimal {
        let body = serde_json::json!({
            "bech32_addr": wallet.get_address("8e"),
            "x25519_sk_hex": wallet.x25519_sk_hex().unwrap(),
            "ecdsa_pk_hex": wallet.encoded_public_key(),
            "scan_limit": 5000
        });
        let resp = client
            .post(format!("{}/wallet/balance", base_url))
            .json(&body)
            .send()
            .await
            .unwrap();
        let json: serde_json::Value = resp.json().await.unwrap();
        Decimal::from_str(json["balance"].as_str().unwrap_or("0")).unwrap_or(Decimal::ZERO)
    }

    // === HELPER: Get balance by address only (for admin with no private keys) ===
    async fn get_balance_by_addr(client: &Client, base_url: &str, addr: &str) -> Decimal {
        // For Plain payloads, we can query with just the address (no decryption keys needed)
        let body = serde_json::json!({
            "bech32_addr": addr,
            "scan_limit": 5000
        });
        let resp = client
            .post(format!("{}/wallet/balance", base_url))
            .json(&body)
            .send()
            .await
            .unwrap();
        let json: serde_json::Value = resp.json().await.unwrap();
        Decimal::from_str(json["balance"].as_str().unwrap_or("0")).unwrap_or(Decimal::ZERO)
    }

    // === HELPER: Get UTXOs ===
    async fn get_utxos(
        client: &Client,
        base_url: &str,
        wallet: &Wallet,
    ) -> Vec<(String, u32, Decimal)> {
        let body = serde_json::json!({
            "bech32_addr": wallet.get_address("8e"),
            "x25519_sk_hex": wallet.x25519_sk_hex().unwrap(),
            "ecdsa_pk_hex": wallet.encoded_public_key(),
            "scan_limit": 5000
        });
        let resp = client
            .post(format!("{}/wallet/balance", base_url))
            .json(&body)
            .send()
            .await
            .unwrap();
        let json: serde_json::Value = resp.json().await.unwrap();
        json["utxos"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|u| {
                (
                    u["txid"].as_str().unwrap().to_string(),
                    u["index"].as_u64().unwrap() as u32,
                    Decimal::from_str(u["amount"].as_str().unwrap()).unwrap(),
                )
            })
            .collect()
    }

    // === HELPER: Send transaction ===
    async fn send_tx(
        client: &Client,
        base_url: &str,
        sender: &Wallet,
        sender_addr: &str,
        utxo: (String, u32, Decimal),
        outputs: Vec<(&str, &str)>, // (addr, amount)
        fee: &str,
        admin_addr: &str,
        parents: Vec<String>,
    ) -> Result<String> {
        let input = TxInput {
            out: OutputId {
                txid: utxo.0,
                index: utxo.1,
            },
        };

        let mut tx_outputs: Vec<TxOutput> = outputs
            .iter()
            .map(|(addr, amt)| TxOutput {
                address: addr.to_string(),
                amount: amt.to_string(),
            })
            .collect();

        // Add fee output to Admin
        tx_outputs.push(TxOutput {
            address: admin_addr.to_string(),
            amount: fee.to_string(),
        });

        let mut tx = Transaction {
            inputs: vec![input],
            outputs: tx_outputs,
            fee: "0.0".to_string(), // Fee is implicit via Admin output
            unlocks: vec![],
        };

        let msg_hex = tx.signing_message()?;
        let sig_b64 = sender.sign(&msg_hex)?;
        tx.unlocks.push(Unlock {
            pubkey_hex: sender.encoded_public_key(),
            signature_b64: sig_b64,
        });

        println!("DEBUG send_tx: msg_hex={}", &msg_hex[..20]);
        println!(
            "DEBUG send_tx: pubkey={}",
            &sender.encoded_public_key()[..20]
        );
        println!("DEBUG send_tx: tx={}", serde_json::to_string(&tx).unwrap());

        let payload = Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx)));
        let mut block = Block::new(parents, payload, 0, None, compute_block_id).unwrap();
        loop {
            if block.id.starts_with("0000") {
                break;
            }
            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
        }

        let mut wb = WireBlock {
            id: block.id.clone(),
            parents: block.parents.clone(),
            payload_json: Some(serde_json::to_string(&block.payload)?),
            nonce: block.nonce,
            network_id: "pms-test".to_string(),
            protocol_version: 1,
            signer_pk_hex: sender.encoded_public_key(),
            signature_hex: String::new(),
            metadata: None,
        };
        wb.signature_hex = sender.sign(&canonical_wireblock_message(&wb)).unwrap();

        let resp = client
            .post(format!("{}/submit/block", base_url))
            .json(&wb)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await?;
            println!("DEBUG send_tx: status={} body={}", status, err);
            anyhow::bail!("Tx failed: {} - {}", status, err);
        }

        Ok(wb.id)
    }

    // === STEP 1: Alice mines 3 blocks (150 PMS) ===
    println!("\n📦 Step 1: Alice mines 3 blocks...");
    let mut parents = get_tips(&client, &base_url).await;
    for _ in 0..3 {
        let new_id = mine_mint(
            &client,
            &base_url,
            &alice,
            &alice_addr,
            "50.0",
            parents.clone(),
        )
        .await?;
        parents = vec![new_id];
        sleep(Duration::from_millis(100)).await;
    }
    sleep(Duration::from_secs(1)).await;

    // === STEP 2: Alice → Bob (25.12345678 + 0.001 fee) ===
    println!("💸 Step 2: Alice → Bob (25.12345678)...");
    let alice_utxos = get_utxos(&client, &base_url, &alice).await;
    let utxo = alice_utxos[0].clone();
    // Input: 50, Output: Bob 25.12345678, Alice change: 24.87654321, Fee: 0.001
    let new_id = send_tx(
        &client,
        &base_url,
        &alice,
        &alice_addr,
        utxo,
        vec![(&bob_addr, "25.12345678"), (&alice_addr, "24.87554321")],
        "0.00100001",
        &admin_addr,
        parents.clone(),
    )
    .await?;
    parents = vec![new_id];
    sleep(Duration::from_millis(200)).await;

    // === STEP 3: Bob mines 2 blocks (+100) ===
    println!("📦 Step 3: Bob mines 2 blocks...");
    for _ in 0..2 {
        let new_id =
            mine_mint(&client, &base_url, &bob, &bob_addr, "50.0", parents.clone()).await?;
        parents = vec![new_id];
        sleep(Duration::from_millis(100)).await;
    }
    sleep(Duration::from_secs(1)).await;

    // === STEP 4: Bob → Carol (50.00000001 + 0.00000001 fee) ===
    println!("💸 Step 4: Bob → Carol (50.00000001)...");
    let bob_utxos = get_utxos(&client, &base_url, &bob).await;
    let utxo = bob_utxos
        .iter()
        .find(|u| u.2 >= Decimal::from_str("50").unwrap())
        .unwrap()
        .clone();
    // Bob has 50 from mining. Use one.
    let new_id = send_tx(
        &client,
        &base_url,
        &bob,
        &bob_addr,
        utxo.clone(),
        vec![(&carol_addr, "49.99999998")],
        "0.00000002",
        &admin_addr,
        parents.clone(),
    )
    .await?;
    parents = vec![new_id];
    sleep(Duration::from_millis(200)).await;

    // === STEP 5: Alice → Carol (10.5 + 0.123 fee) ===
    println!("💸 Step 5: Alice → Carol (10.5)...");
    let alice_utxos = get_utxos(&client, &base_url, &alice).await;
    let utxo = alice_utxos[0].clone();
    // Input: 50 or 24.xxx change. Use first available.
    let input_amt = utxo.2;
    let change =
        input_amt - Decimal::from_str("10.5").unwrap() - Decimal::from_str("0.123").unwrap();
    let new_id = send_tx(
        &client,
        &base_url,
        &alice,
        &alice_addr,
        utxo,
        vec![(&carol_addr, "10.5"), (&alice_addr, &change.to_string())],
        "0.123",
        &admin_addr,
        parents.clone(),
    )
    .await?;
    parents = vec![new_id];
    sleep(Duration::from_millis(200)).await;

    // === STEP 6: Carol → Dave (30.25 + 0.001 fee) ===
    println!("💸 Step 6: Carol → Dave (30.25)...");
    let carol_utxos = get_utxos(&client, &base_url, &carol).await;
    let utxo = carol_utxos[0].clone();
    let input_amt = utxo.2;
    let change =
        input_amt - Decimal::from_str("30.25").unwrap() - Decimal::from_str("0.001").unwrap();
    let new_id = send_tx(
        &client,
        &base_url,
        &carol,
        &carol_addr,
        utxo,
        vec![(&dave_addr, "30.25"), (&carol_addr, &change.to_string())],
        "0.001",
        &admin_addr,
        parents.clone(),
    )
    .await?;
    parents = vec![new_id];
    sleep(Duration::from_millis(200)).await;

    // === STEP 7: Carol mines 1 block (+50) ===
    println!("📦 Step 7: Carol mines 1 block...");
    let new_id = mine_mint(
        &client,
        &base_url,
        &carol,
        &carol_addr,
        "50.0",
        parents.clone(),
    )
    .await?;
    parents = vec![new_id];
    sleep(Duration::from_secs(1)).await;

    // === STEP 8: Carol → Alice (15.0 + 0.01 fee) ===
    println!("💸 Step 8: Carol → Alice (15.0)...");
    let carol_utxos = get_utxos(&client, &base_url, &carol).await;
    let utxo = carol_utxos
        .iter()
        .find(|u| u.2 >= Decimal::from_str("15.01").unwrap())
        .unwrap()
        .clone();
    let input_amt = utxo.2;
    let change =
        input_amt - Decimal::from_str("15.0").unwrap() - Decimal::from_str("0.01").unwrap();
    let _new_id = send_tx(
        &client,
        &base_url,
        &carol,
        &carol_addr,
        utxo,
        vec![(&alice_addr, "15.0"), (&carol_addr, &change.to_string())],
        "0.01",
        &admin_addr,
        parents.clone(),
    )
    .await?;

    sleep(Duration::from_secs(2)).await;

    // === VERIFICATION ===
    println!("\n✅ Verifying final balances...");

    let bal_alice = get_balance(&client, &base_url, &alice).await;
    let bal_bob = get_balance(&client, &base_url, &bob).await;
    let bal_carol = get_balance(&client, &base_url, &carol).await;
    let bal_dave = get_balance(&client, &base_url, &dave).await;
    let bal_admin = get_balance(&client, &base_url, &admin).await;

    println!("💰 Alice: {}", bal_alice);
    println!("💰 Bob:   {}", bal_bob);
    println!("💰 Carol: {}", bal_carol);
    println!("💰 Dave:  {}", bal_dave);
    println!("💼 Admin (fees): {}", bal_admin);

    // Dave should have exactly 30.25
    assert_eq!(
        bal_dave,
        Decimal::from_str("30.25").unwrap(),
        "Dave balance mismatch"
    );

    // Admin should have sum of fees: 0.00100001 + 0.00000002 + 0.123 + 0.001 + 0.01 = 0.13500003
    let expected_admin = Decimal::from_str("0.13500003").unwrap();
    assert_eq!(bal_admin, expected_admin, "Admin fee balance mismatch");

    println!("\n🎉 Scenario 2 (Enhanced) PASSED!");
    Ok(())
}

// ============================================================================
// E2E SCENARIO 3: HISTORY VERIFICATION
// ============================================================================
// This test verifies that the /wallet/history endpoint correctly returns
// transaction history for sender and recipient addresses.
// ============================================================================

#[tokio::test]
#[ignore]
async fn e2e_scenario_3_history() -> Result<()> {
    use pms_types::{Block, OutputId, PlainPayload, Transaction, TxInput, TxOutput, Unlock};
    use pms_wallet::SignerBackend;
    use rust_decimal::Decimal;
    use std::str::FromStr; // Import helper trait for sign()

    let base_url = env::var("PMS_API_URL").unwrap_or_else(|_| "https://127.0.0.1:8080".to_string());
    println!("🚀 E2E Scenario 3: History Verification");
    println!("   Target: {}", base_url);

    let client = {
        // We load the CA certificate that signed the server's cert
        let cert_pem = std::fs::read("secrets/tls/ca-cert.pem")
            .or_else(|_| std::fs::read("../../secrets/tls/ca-cert.pem"))
            .expect("Failed to read CA cert from secrets/tls/ca-cert.pem");
        let cert = reqwest::Certificate::from_pem(&cert_pem)?;
        Client::builder()
            .add_root_certificate(cert)
            .danger_accept_invalid_certs(false)
            .build()?
    };

    // === WALLETS ===
    let alice = Wallet::generate();
    let bob = Wallet::generate();

    let alice_addr = alice.get_address("8e");
    let bob_addr = bob.get_address("8e");

    println!("👤 Alice: {}", &alice_addr[..20]);
    println!("👤 Bob:   {}", &bob_addr[..20]);

    // === HELPER: Get history ===
    async fn get_history(client: &Client, base_url: &str, addr: &str) -> serde_json::Value {
        let body = serde_json::json!({
            "bech32_addr": addr,
            "limit": 100
        });
        let resp = client
            .post(format!("{}/wallet/history", base_url))
            .json(&body)
            .send()
            .await
            .unwrap();
        resp.json().await.unwrap_or_default()
    }

    // === HELPER: Mine Mint block ===
    async fn mine_mint_s3(
        client: &Client,
        base_url: &str,
        miner: &Wallet,
        to_addr: &str,
        amount: &str,
        parents: Vec<String>,
    ) -> Result<String> {
        let output = TxOutput {
            address: to_addr.to_string(),
            amount: amount.to_string(),
        };
        let payload = Some(PayloadEnvelope::Plain(PlainPayload::Mint {
            outputs: vec![output],
        }));

        let mut block = Block::new(parents, payload, 0, None, compute_block_id).unwrap();
        loop {
            if block.id.starts_with("0000") {
                break;
            }
            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
        }

        let mut wb = WireBlock {
            id: block.id.clone(),
            parents: block.parents.clone(),
            payload_json: Some(serde_json::to_string(&block.payload)?),
            nonce: block.nonce,
            network_id: "pms-test".to_string(),
            protocol_version: 1,
            signer_pk_hex: miner.encoded_public_key(),
            signature_hex: String::new(),
            metadata: None,
        };
        wb.signature_hex = miner.sign(&canonical_wireblock_message(&wb)).unwrap();

        client
            .post(format!("{}/submit/block", base_url))
            .json(&wb)
            .send()
            .await?
            .error_for_status()?;

        Ok(wb.id)
    }

    // === HELPER: Send transaction ===
    async fn send_tx_s3(
        client: &Client,
        base_url: &str,
        sender: &Wallet,
        sender_addr: &str,
        utxo_txid: &str,
        utxo_index: u32,
        recipient_addr: &str,
        amount: &str,
        change_addr: &str,
        change_amount: &str,
        parents: Vec<String>,
    ) -> Result<String> {
        let mut tx = Transaction {
            inputs: vec![TxInput {
                out: OutputId {
                    txid: utxo_txid.to_string(),
                    index: utxo_index,
                },
            }],
            outputs: vec![
                TxOutput {
                    address: recipient_addr.to_string(),
                    amount: amount.to_string(),
                },
                TxOutput {
                    address: change_addr.to_string(),
                    amount: change_amount.to_string(),
                },
            ],
            fee: "0.0".to_string(),
            unlocks: vec![],
        };

        // Sign
        let msg = tx.signing_message().unwrap();
        // let msg_bytes = hex::decode(&msg).expect("Invalid signing message hex");
        let sig_b64 = sender.sign(&msg).unwrap(); // Use sign() interface directly
        tx.unlocks = vec![Unlock {
            pubkey_hex: sender.encoded_public_key(),
            signature_b64: sig_b64,
        }];

        let payload = Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx)));
        let mut block = Block::new(parents, payload, 0, None, compute_block_id).unwrap();
        loop {
            if block.id.starts_with("0000") {
                break;
            }
            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
        }

        let mut wb = WireBlock {
            id: block.id.clone(),
            parents: block.parents.clone(),
            payload_json: Some(serde_json::to_string(&block.payload)?),
            nonce: block.nonce,
            network_id: "pms-test".to_string(),
            protocol_version: 1,
            signer_pk_hex: sender.encoded_public_key(),
            signature_hex: String::new(),
            metadata: None,
        };
        wb.signature_hex = sender.sign(&canonical_wireblock_message(&wb)).unwrap();

        client
            .post(format!("{}/submit/block", base_url))
            .json(&wb)
            .send()
            .await?
            .error_for_status()?;

        Ok(wb.id)
    }

    // === STEP 1: Alice mines a block ===
    println!("\n📦 Step 1: Alice mines a block...");
    let parents = vec![Block::genesis(compute_block_id).id];
    let mint_id = mine_mint_s3(&client, &base_url, &alice, &alice_addr, "50.0", parents).await?;
    println!("   Minted block: {}", &mint_id[..16]);

    sleep(Duration::from_secs(1)).await;

    // === STEP 2: Alice -> Bob (10 PMS) ===
    println!("💸 Step 2: Alice → Bob (10 PMS)...");
    let tx_id = send_tx_s3(
        &client,
        &base_url,
        &alice,
        &alice_addr,
        &mint_id,
        0,
        &bob_addr,
        "10.0",
        &alice_addr,
        "40.0",
        vec![mint_id.clone()],
    )
    .await?;
    println!("   TX block: {}", &tx_id[..16]);

    sleep(Duration::from_secs(2)).await;

    // === VERIFICATION: Check history for Alice and Bob ===
    println!("\n✅ Verifying history...");

    let alice_history = get_history(&client, &base_url, &alice_addr).await;
    let bob_history = get_history(&client, &base_url, &bob_addr).await;

    println!("📜 Alice history count: {}", alice_history["count"]);
    println!("📜 Bob history count: {}", bob_history["count"]);

    // Alice should have at least 2 entries: Mint + Tx (change back)
    let alice_count = alice_history["count"].as_u64().unwrap_or(0);
    assert!(
        alice_count >= 2,
        "Alice should have at least 2 history entries, got {}",
        alice_count
    );

    // Bob should have at least 1 entry: the Tx receiving 10 PMS
    let bob_count = bob_history["count"].as_u64().unwrap_or(0);
    assert!(
        bob_count >= 1,
        "Bob should have at least 1 history entry, got {}",
        bob_count
    );

    // Verify Bob's history contains a TxUtxo with 10.0 for his address
    let bob_items = bob_history["items"]
        .as_array()
        .expect("items should be array");
    let has_tx = bob_items.iter().any(|item| {
        item["payload_type"] == "TxUtxo"
            && item["payload"]["outputs"]
                .as_array()
                .map_or(false, |outputs| {
                    outputs
                        .iter()
                        .any(|o| o["address"] == bob_addr && o["amount"] == "10.0")
                })
    });
    assert!(
        has_tx,
        "Bob's history should contain a TxUtxo with 10.0 PMS"
    );

    println!("\n🎉 Scenario 3 (History) PASSED!");
    Ok(())
}

#[tokio::test]
#[ignore]
async fn e2e_scenario_4_double_spend() -> Result<()> {
    use pms_types::{Block, OutputId, PlainPayload, Transaction, TxInput, TxOutput, Unlock};
    use pms_wallet::SignerBackend;
    use std::time::Duration;

    let base_url = env::var("PMS_API_URL").unwrap_or_else(|_| "https://127.0.0.1:8080".to_string());
    println!("🚀 E2E Scenario 4: Double Spend Validation");
    println!("   Target: {}", base_url);

    let client = {
        let cert_pem = std::fs::read("secrets/tls/ca-cert.pem")
            .or_else(|_| std::fs::read("../../secrets/tls/ca-cert.pem"))
            .expect("Failed to read CA cert from secrets/tls/ca-cert.pem");
        let cert = reqwest::Certificate::from_pem(&cert_pem)?;
        Client::builder()
            .add_root_certificate(cert)
            .danger_accept_invalid_certs(false)
            .build()?
    };

    let alice = Wallet::generate();
    let bob = Wallet::generate();
    let carol = Wallet::generate();

    let alice_addr = alice.get_address("8e");
    let bob_addr = bob.get_address("8e");
    let carol_addr = carol.get_address("8e");

    println!("👤 Alice: {}", &alice_addr[..20]);
    println!("👤 Bob:   {}", &bob_addr[..20]);
    println!("👤 Carol: {}", &carol_addr[..20]);

    // === HELPER: Mine Mint block (Auto-Submit) ===
    async fn mine_mint_s4(
        client: &Client,
        base_url: &str,
        miner: &Wallet,
        to_addr: &str,
        amount: &str,
        parents: Vec<String>,
    ) -> Result<String> {
        let output = TxOutput {
            address: to_addr.to_string(),
            amount: amount.to_string(),
        };
        let payload = Some(PayloadEnvelope::Plain(PlainPayload::Mint {
            outputs: vec![output],
        }));

        let mut block = Block::new(parents, payload, 0, None, compute_block_id).unwrap();
        loop {
            if block.id.starts_with("0000") {
                break;
            }
            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
        }

        let mut wb = WireBlock {
            id: block.id.clone(),
            parents: block.parents.clone(),
            payload_json: Some(serde_json::to_string(&block.payload)?),
            nonce: block.nonce,
            network_id: "pms-test".to_string(),
            protocol_version: 1,
            signer_pk_hex: miner.encoded_public_key(),
            signature_hex: String::new(),
            metadata: None,
        };
        wb.signature_hex = miner.sign(&canonical_wireblock_message(&wb)).unwrap();

        client
            .post(format!("{}/submit/block", base_url))
            .json(&wb)
            .send()
            .await?
            .error_for_status()?;

        Ok(wb.id)
    }

    // === HELPER: Forge Tx Block (NO SUBMIT) ===
    fn forge_tx_block(
        sender: &Wallet,
        utxo_txid: &str,
        utxo_index: u32,
        recipient_addr: &str,
        amount: &str,
        parents: Vec<String>,
    ) -> WireBlock {
        let tx = Transaction {
            inputs: vec![TxInput {
                out: OutputId {
                    txid: utxo_txid.to_string(),
                    index: utxo_index,
                },
            }],
            outputs: vec![TxOutput {
                address: recipient_addr.to_string(),
                amount: amount.to_string(),
            }],
            fee: "0.0".to_string(),
            unlocks: vec![],
        };

        let msg = tx.signing_message().unwrap();
        let sig_b64 = sender.sign(&msg).unwrap();

        let mut signed_tx = tx;
        signed_tx.unlocks = vec![Unlock {
            pubkey_hex: sender.encoded_public_key(),
            signature_b64: sig_b64,
        }];

        let payload = Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(signed_tx)));
        let mut block = Block::new(parents, payload, 0, None, compute_block_id).unwrap();
        loop {
            if block.id.starts_with("0000") {
                break;
            }
            block.nonce += 1;
            block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
        }

        let mut wb = WireBlock {
            id: block.id.clone(),
            parents: block.parents.clone(),
            payload_json: Some(serde_json::to_string(&block.payload).unwrap()),
            nonce: block.nonce,
            network_id: "pms-test".to_string(),
            protocol_version: 1,
            signer_pk_hex: sender.encoded_public_key(),
            signature_hex: String::new(),
            metadata: None,
        };
        wb.signature_hex = sender.sign(&canonical_wireblock_message(&wb)).unwrap();
        wb
    }

    // === STEP 1: Alice mines 50 PMS ===
    println!("\n📦 Step 1: Alice mines 50 PMS...");
    let parents = vec![Block::genesis(compute_block_id).id];
    let mint_id = mine_mint_s4(
        &client,
        &base_url,
        &alice,
        &alice_addr,
        "50.0",
        parents.clone(),
    )
    .await?;
    println!("   Minted block: {}", &mint_id[..16]);

    sleep(Duration::from_secs(2)).await;

    // === STEP 2: Forge Tx1 (Alice -> Bob) ===
    println!("🛠️  Forging Tx1: Alice -> Bob...");
    let wb1 = forge_tx_block(
        &alice,
        &mint_id,
        0,
        &bob_addr,
        "50.0",
        vec![mint_id.clone()],
    );
    println!("   Block 1 ID: {}", &wb1.id[..16]);

    // === STEP 3: Forge Tx2 (Alice -> Carol - DOUBLE SPEND) ===
    println!("🛠️  Forging Tx2: Alice -> Carol (SAME UTXO)...");
    let wb2 = forge_tx_block(
        &alice,
        &mint_id,
        0,
        &carol_addr,
        "50.0",
        vec![mint_id.clone()],
    );
    println!("   Block 2 ID: {}", &wb2.id[..16]);

    // === STEP 4: Submit Tx1 (Should Succeed) ===
    println!("🚀 Submitting Tx1 (Valid)...");
    let resp1 = client
        .post(format!("{}/submit/block", base_url))
        .json(&wb1)
        .send()
        .await?;

    assert!(resp1.status().is_success(), "Tx1 submit failed");
    println!("   Tx1 submitted successfully.");

    sleep(Duration::from_millis(500)).await;

    // === STEP 5: Submit Tx2 (Should Fail or be Rejected) ===
    println!("🧨 Submitting Tx2 (Double Spend)...");
    let resp2 = client
        .post(format!("{}/submit/block", base_url))
        .json(&wb2)
        .send()
        .await?;

    println!("   Tx2 Status: {}", resp2.status());

    // Check outcome
    if resp2.status().is_success() {
        println!("⚠️  Tx2 was accepted by API (Async validation?). Checking balances...");
        sleep(Duration::from_secs(3)).await;

        let bob_h = client
            .post(format!("{}/wallet/history", base_url))
            .json(&serde_json::json!({"bech32_addr": bob_addr, "limit": 10}))
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        let carol_h = client
            .post(format!("{}/wallet/history", base_url))
            .json(&serde_json::json!({"bech32_addr": carol_addr, "limit": 10}))
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        let bob_count = bob_h["count"].as_u64().unwrap_or(0);
        let carol_count = carol_h["count"].as_u64().unwrap_or(0);

        println!("   Bob history count: {}", bob_count);
        println!("   Carol history count: {}", carol_count);

        assert_eq!(bob_count, 1, "Bob should have received funds");
        assert_eq!(
            carol_count, 0,
            "Carol should NOT have received funds (Double Spend)"
        );
        println!("✅ Double spend detected and filtered (Post-Validation).");
    } else {
        println!("✅ Tx2 rejected immediately by API (Correct).");
    }

    println!("\n🎉 Scenario 4 (Double Spend) PASSED!");
    Ok(())
}
