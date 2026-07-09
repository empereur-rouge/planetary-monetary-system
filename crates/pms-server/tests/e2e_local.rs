//! E2E Local Test (Dry-Run)
//!
//! Test E2E rapide sans Docker pour développement local:
//! 1. Assume que le serveur est déjà lancé (cargo run)
//! 2. Se connecte en HTTP direct (pas de Gateway, pas de TLS)
//! 3. Single Writer Mode actif (Coordinator signe les blocks)
//! 4. Mints 100 Cubes → Burns → Transfers → Vérifications
//!
//! Usage:
//!   Terminal 1: PMS_CONFIG=etc/config/config.local.toml cargo run --release
//!   Terminal 2: cargo test --test e2e_local -- --ignored --nocapture

use anyhow::{Context, Result};
use base64::Engine;
use hex::FromHex;
use k256::ecdsa::{Signature, SigningKey, signature::Signer};
use pms_types_nft::NftMetadata;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireBlock;
use rand::Rng;
use reqwest::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;
use std::time::Duration;
use tokio::time::sleep;

// ═══════════════════════════════════════════════════════════════════════════
// Types (mêmes que e2e_prod_sim.rs)
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize, Deserialize)]
struct CubeAttributes {
    weight: u32,
    size: u32,
    density: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct CubeExtra {
    rarity: String,
    attributes: CubeAttributes,
    roll: u64,
    signature: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct MintNftRequest {
    token_id: String,
    owner_address: String,
    owner_x25519_pubkey: String,
    metadata: NftMetadata,
    // v0.30.1: the API-key path requires an authorized-issuer signature.
    #[serde(skip_serializing_if = "Option::is_none")]
    creator_pubkey_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    creator_signature_b64: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BurnNftResponse {
    status: String,
    block_id: String,
    token_id: String,
    token_ids: Option<Vec<String>>,
    refund: Option<RefundPreview>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RefundPreview {
    amount: String,
    recipient: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct BalanceResponse {
    balance: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Utxo {
    #[serde(rename = "txId")]
    tx_id: String,
    #[serde(rename = "outIdx")]
    out_idx: u32,
    amount: String,
    address: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct UtxosResponse {
    utxos: Vec<Utxo>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Helper Functions
// ═══════════════════════════════════════════════════════════════════════════

/// Generate random cube attributes
fn generate_cube_attributes() -> CubeAttributes {
    let mut rng = rand::rng();

    CubeAttributes {
        weight: rng.random_range(500..=5000), // 0.5kg to 5kg
        size: rng.random_range(20..=80),      // 2cm to 8cm
        density: rng.random_range(20..=100),  // 0.2 to 1.0
    }
}

/// Generate a unique token_id (64 hex chars)
fn generate_token_id(index: usize) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(b"e2e_local_test_cube_");
    hasher.update(index.to_string().as_bytes());
    hasher.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string()
            .as_bytes(),
    );
    let hash = hasher.finalize();
    hex::encode(hash)
}

/// Sign cube attributes with Authority private key
fn sign_cube_attributes(attrs: &CubeAttributes, authority_sk_hex: &str) -> Result<String> {
    let message = format!(
        "weight:{},size:{},density:{}",
        attrs.weight, attrs.size, attrs.density
    );

    let sk_bytes = <Vec<u8>>::from_hex(authority_sk_hex)?;
    let signing_key =
        SigningKey::from_slice(&sk_bytes).context("Failed to create SigningKey from bytes")?;

    let signature: Signature = signing_key.sign(message.as_bytes());
    let sig_der = signature.to_der();

    Ok(base64::engine::general_purpose::STANDARD.encode(sig_der.as_bytes()))
}

/// Generate a random Authority ECDSA keypair and return (private_key_hex, public_key_hex)
fn generate_authority_keypair() -> (String, String) {
    use rand::RngCore;

    let mut rng = rand::rng();
    let mut secret_bytes = [0u8; 32];
    rng.fill_bytes(&mut secret_bytes);

    let signing_key = SigningKey::from_slice(&secret_bytes).expect("Failed to create signing key");
    let verifying_key = signing_key.verifying_key();

    let sk_hex = hex::encode(signing_key.to_bytes());
    let pk_hex = hex::encode(verifying_key.to_encoded_point(true).as_bytes());

    (sk_hex, pk_hex)
}

// ═══════════════════════════════════════════════════════════════════════════
// Main Test
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore] // Run with: cargo test --test e2e_local -- --ignored --nocapture
async fn e2e_local_simulation() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info,pms_server=debug")
        .init();

    println!("\n🏠 E2E Local Test (Dry-Run - No Docker)");
    println!("══════════════════════════════════════════════════════════════\n");

    // ─────────────────────────────────────────────────────────────────────────
    // Phase 0: Pre-requisites
    // ─────────────────────────────────────────────────────────────────────────
    println!("⚠️  PRE-REQUISITES:");
    println!("   1. Start server: PMS_CONFIG=etc/config/config.local.toml cargo run --release");
    println!("   2. Server must be running on http://127.0.0.1:8080");
    println!("   3. Press ENTER when ready...\n");

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    // ─────────────────────────────────────────────────────────────────────────
    // Phase 1: Verify Server Connection
    // ─────────────────────────────────────────────────────────────────────────
    println!("📋 Phase 1: Verify Server Connection");

    let base_url = "http://127.0.0.1:8080"; // Direct Engine, no Gateway, no TLS
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;

    // Try to connect
    let health_resp = client
        .get(format!("{}/healthz", base_url))
        .send()
        .await
        .context("Failed to connect to server. Is it running?")?;

    if !health_resp.status().is_success() {
        anyhow::bail!(
            "Server health check failed. Status: {}",
            health_resp.status()
        );
    }

    println!("  ✅ Server is running and responding\n");

    // ─────────────────────────────────────────────────────────────────────────
    // Phase 2: Generate Wallets & Authority Key
    // ─────────────────────────────────────────────────────────────────────────
    println!("📋 Phase 2: Generate Wallets & Authority Key");

    let wallet_a = Wallet::from_seed(&[100u8; 32], None)
        .map_err(|e| anyhow::anyhow!("Wallet A error: {}", e))?;
    let wallet_b = Wallet::from_seed(&[200u8; 32], None)
        .map_err(|e| anyhow::anyhow!("Wallet B error: {}", e))?;

    // Coordinator wallet (for Single Writer Mode)
    let coordinator_sk_hex = "52f4cb8344e318c120f87bc0efb429bdd6b379c700731af27aaf59efffc0b248";
    let coordinator_sk_bytes = <Vec<u8>>::from_hex(coordinator_sk_hex)?;
    let coordinator_seed: [u8; 32] = coordinator_sk_bytes[..]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid coordinator key length"))?;
    let wallet_coordinator = Wallet::from_seed(&coordinator_seed, None)
        .map_err(|e| anyhow::anyhow!("Coordinator wallet error: {}", e))?;
    let addr_coordinator = wallet_coordinator.encoded_public_key();

    let addr_a = wallet_a.encoded_public_key();
    let addr_b = wallet_b.encoded_public_key();
    let x25519_a = wallet_a.x25519_pub_hex().to_string();

    // Generate Authority keypair (note: in production, this is pre-configured)
    let (authority_sk, _authority_pk) = generate_authority_keypair();

    println!(
        "  👤 Wallet A: {}...{}",
        &addr_a[..16],
        &addr_a[addr_a.len() - 8..]
    );
    println!(
        "  👤 Wallet B: {}...{}",
        &addr_b[..16],
        &addr_b[addr_b.len() - 8..]
    );
    println!(
        "  🔧 Coordinator: {}...{}",
        &addr_coordinator[..16],
        &addr_coordinator[addr_coordinator.len() - 8..]
    );

    // ─────────────────────────────────────────────────────────────────────────
    // Phase 3: Mint 100 Cubes
    // ─────────────────────────────────────────────────────────────────────────
    println!("\n📋 Phase 3: Mint 100 Cubes with Authority Signatures");

    let mut minted_token_ids = Vec::new();

    for i in 0..100 {
        let token_id = generate_token_id(i);
        let attributes = generate_cube_attributes();

        // Sign attributes with Authority key
        let signature = sign_cube_attributes(&attributes, &authority_sk)?;

        let cube_extra = CubeExtra {
            rarity: "common".to_string(),
            attributes,
            roll: 42,
            signature,
        };

        let metadata = NftMetadata {
            name: Some(format!("Cube #{}", i)),
            description: Some("E2E Test Cube".to_string()),
            uri: None,
            nft_type: Some("cube".to_string()),
            extra: Some(serde_json::to_string(&cube_extra)?),
        };

        // v0.30.1: /v1/nft/mint requires an authorized-issuer signature on the
        // API-key path — sign with the coordinator (an authorized issuer).
        let nft_msg = pms_server::api_fn::nft::nft_mint_signing_message(
            "pms-e2e-test",
            "main",
            &token_id,
            &addr_a,
            &metadata,
        );
        let nft_sig = wallet_coordinator.sign(&nft_msg)?;
        let mint_req = MintNftRequest {
            token_id: token_id.clone(),
            owner_address: addr_a.clone(),
            owner_x25519_pubkey: x25519_a.clone(),
            metadata,
            creator_pubkey_hex: Some(wallet_coordinator.public_key_hex.clone()),
            creator_signature_b64: Some(nft_sig),
        };

        let resp = client
            .post(format!("{}/v1/nft/mint", base_url))
            .json(&mint_req)
            .send()
            .await
            .with_context(|| format!("Failed to send mint request for cube {}", i))?;

        let status = resp.status();
        if !status.is_success() {
            let error_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "No error message".to_string());
            anyhow::bail!("Mint {} failed with status {}: {}", i, status, error_text);
        }

        let _response_body = resp.text().await?;

        minted_token_ids.push(token_id);

        if (i + 1) % 10 == 0 {
            println!("  🎲 Minted {}/100 cubes", i + 1);
        }

        // Small delay to pace requests (100ms per mint = 10s total)
        sleep(Duration::from_millis(100)).await;
    }

    println!("  ✅ Successfully minted 100 cubes");

    // Verify ownership
    let balance_a_before = client
        .post(format!("{}/v1/balance", base_url))
        .json(&json!({"address": addr_a}))
        .send()
        .await?
        .json::<BalanceResponse>()
        .await?;

    println!(
        "  💰 Wallet A balance before burn: {} PMS",
        balance_a_before.balance
    );

    // ─────────────────────────────────────────────────────────────────────────
    // Phase 4: Burn All Cubes
    // ─────────────────────────────────────────────────────────────────────────
    println!("\n📋 Phase 4: Burn All 100 Cubes");

    // Get tips for parents
    let tips_resp: Vec<String> = client
        .post(format!("{}/v1/dag/tips", base_url))
        .json(&json!({"limit": 2}))
        .send()
        .await?
        .json()
        .await?;

    let parents = if tips_resp.is_empty() {
        vec!["0000000000000000000000000000000000000000000000000000000000000000".to_string()]
    } else {
        tips_resp
    };

    // Create BatchBurn payload
    let burn_action = json!({
        "BatchBurn": {
            "token_ids": minted_token_ids.clone(),
            "burner": addr_a
        }
    });

    let payload = json!({
        "Plain": {
            "Nft": burn_action
        }
    });

    // Create and sign WireBlock (Coordinator signs in Single Writer Mode)
    let payload_str = serde_json::to_string(&payload)?;

    // Compute block ID
    use pms_utils::compute_block_id;
    let block_id = compute_block_id(&parents, &Some(serde_json::from_str(&payload_str)?), 0);

    let mut wire_block = WireBlock {
        id: block_id.clone(),
        parents: parents.clone(),
        payload_json: Some(payload_str),
        nonce: 0,
        network_id: "pms-e2e-test".to_string(),
        protocol_version: 1,
        signer_pk_hex: addr_coordinator.clone(), // Coordinator signs in Single Writer Mode
        signature_hex: String::new(),
        metadata: None,
    };

    // Sign the block with Coordinator wallet (Single Writer Mode)
    use pms_wallet::signing_wire::canonical_wireblock_message;
    let msg = canonical_wireblock_message(&wire_block);
    wire_block.signature_hex = wallet_coordinator.sign(&msg)?;

    // Submit burn
    let burn_resp = client
        .post(format!("{}/v1/nft/burn", base_url))
        .json(&wire_block)
        .send()
        .await?;

    if !burn_resp.status().is_success() {
        let error_text = burn_resp.text().await?;
        anyhow::bail!("Burn failed: {}", error_text);
    }

    let burn_result: BurnNftResponse = burn_resp.json().await?;

    println!("  🔥 Burn Status: {}", burn_result.status);
    println!("  📦 Block ID: {}...", &burn_result.block_id[..16]);

    if let Some(refund) = &burn_result.refund {
        println!("  💰 Total Refund: {} PMS", refund.amount);
        println!(
            "  👤 Recipient: {}...{}",
            &refund.recipient[..16],
            &refund.recipient[refund.recipient.len() - 8..]
        );
    }

    // Verify balance increased
    sleep(Duration::from_secs(1)).await; // Wait for balance update

    let balance_a_after = client
        .post(format!("{}/v1/balance", base_url))
        .json(&json!({"address": addr_a}))
        .send()
        .await?
        .json::<BalanceResponse>()
        .await?;

    println!(
        "  💰 Wallet A balance after burn: {} PMS",
        balance_a_after.balance
    );

    // ─────────────────────────────────────────────────────────────────────────
    // Phase 5: Transfer PMS from A to B
    // ─────────────────────────────────────────────────────────────────────────
    println!("\n📋 Phase 5: Transfer PMS from Wallet A to B");

    let transfer_amount = "50.0";

    // Get UTXOs for Wallet A
    let utxos_resp: UtxosResponse = client
        .get(format!("{}/v1/wallet/{}/utxos", base_url, addr_a))
        .send()
        .await?
        .json()
        .await?;

    println!("  📊 Wallet A has {} UTXOs", utxos_resp.utxos.len());

    // Build transaction
    let tx_payload = json!({
        "Plain": {
            "Tx": {
                "inputs": utxos_resp.utxos.iter().map(|u| {
                    json!({
                        "txId": u.tx_id,
                        "outIdx": u.out_idx
                    })
                }).collect::<Vec<_>>(),
                "outputs": [
                    {
                        "address": addr_b,
                        "amount": transfer_amount
                    }
                ]
            }
        }
    });

    // Get fresh tips
    let tips_resp: Vec<String> = client
        .post(format!("{}/v1/dag/tips", base_url))
        .json(&json!({"limit": 2}))
        .send()
        .await?
        .json()
        .await?;

    let parents = if tips_resp.is_empty() {
        vec!["0000000000000000000000000000000000000000000000000000000000000000".to_string()]
    } else {
        tips_resp
    };

    let payload_str = serde_json::to_string(&tx_payload)?;
    let block_id = compute_block_id(&parents, &Some(serde_json::from_str(&payload_str)?), 0);

    let mut tx_wire_block = WireBlock {
        id: block_id.clone(),
        parents,
        payload_json: Some(payload_str),
        nonce: 0,
        network_id: "pms-e2e-test".to_string(),
        protocol_version: 1,
        signer_pk_hex: addr_coordinator.clone(), // Coordinator signs in Single Writer Mode
        signature_hex: String::new(),
        metadata: None,
    };

    let msg = canonical_wireblock_message(&tx_wire_block);
    tx_wire_block.signature_hex = wallet_coordinator.sign(&msg)?;

    // Submit transaction
    let tx_resp = client
        .post(format!("{}/submit/block", base_url))
        .json(&tx_wire_block)
        .send()
        .await?;

    if !tx_resp.status().is_success() {
        let error_text = tx_resp.text().await?;
        anyhow::bail!("Transfer failed: {}", error_text);
    }

    println!("  ✅ Transfer submitted successfully");

    // Verify balances
    sleep(Duration::from_secs(1)).await;

    let balance_b = client
        .post(format!("{}/v1/balance", base_url))
        .json(&json!({"address": addr_b}))
        .send()
        .await?
        .json::<BalanceResponse>()
        .await?;

    println!("  💰 Wallet B balance: {} PMS", balance_b.balance);

    let balance_b_decimal = Decimal::from_str(&balance_b.balance)?;
    let transfer_decimal = Decimal::from_str(transfer_amount)?;

    assert!(
        balance_b_decimal >= transfer_decimal,
        "Wallet B should have received at least {} PMS, got {}",
        transfer_amount,
        balance_b.balance
    );

    // ─────────────────────────────────────────────────────────────────────────
    // Phase 6: Summary
    // ─────────────────────────────────────────────────────────────────────────
    println!("\n📋 Phase 6: Test Summary");
    println!("  ✅ Minted 100 Cubes");
    println!("  ✅ Burned all Cubes");
    println!("  ✅ Transferred PMS tokens");
    println!("  ✅ All phases completed successfully!");

    println!("\n══════════════════════════════════════════════════════════════");
    println!("  ✅ E2E Local Test PASSED");
    println!("══════════════════════════════════════════════════════════════\n");

    Ok(())
}
