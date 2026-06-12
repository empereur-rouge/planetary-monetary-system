// stress_common.rs - Shared utilities for stress tests
// This module is used by both local_stress.rs and docker_stress_sync.rs
// to ensure identical transaction logic.

use anyhow::Result;
use pms_types::{
    Block, OutputId, PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput, Unlock,
};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet, signing_wire::canonical_wireblock_message};
use pms_wire::WireBlock;
use reqwest::Client;
use std::str::FromStr;
use tokio::time::{Duration, sleep};

// === CONSTANTS (Production values) ===

/// Fee ratio: 2.4% of payment amount
pub const FEE_RATIO: &str = "0.024";

/// PoW difficulty prefix (2 hex chars = 8 bits)
pub const POW_PREFIX: &str = "00";

/// Network ID for tests
pub const NETWORK_ID: &str = "pms-test";

/// Protocol version
pub const PROTOCOL_VERSION: u32 = 1;

/// Coordinator Private Key for Docker tests (corresponds to node1.key)
/// This is the private key that matches COORDINATOR_PUBLIC_KEY in config.docker-test.toml
/// Public Key: 03a1829d324538dee1265c879b720e42e15d19a4dbf464f0943a03b5d29aad02ff
pub const COORDINATOR_PRIVATE_KEY_HEX: &str =
    "52f4cb8344e318c120f87bc0efb429bdd6b379c700731af27aaf59efffc0b248";

// === TYPES ===

/// Reference to an unspent output
#[derive(Debug, Clone)]
pub struct OutputRef {
    pub txid: String,
    pub index: u32,
    pub amount: rust_decimal::Decimal,
}

// === HELPERS ===

/// Creates a Coordinator Wallet from the testnet private key
/// This is the ONLY wallet authorized to mint on testnet
pub fn make_coordinator_wallet() -> Wallet {
    Wallet::from_hex(COORDINATOR_PRIVATE_KEY_HEX).expect("Invalid Coordinator private key")
}

/// Calculate fee based on production ratio (2.4%)
pub fn calculate_fee(payment_amount: &str) -> rust_decimal::Decimal {
    let payment = rust_decimal::Decimal::from_str(payment_amount).unwrap();
    let ratio = rust_decimal::Decimal::from_str(FEE_RATIO).unwrap();
    payment * ratio
}

/// Mine a PoW-valid block with the given prefix
pub fn mine_pow(block: &mut Block) {
    while !block.id.starts_with(POW_PREFIX) {
        block.nonce += 1;
        block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
    }
}

/// Attempt to mint and expect FAILURE (for unauthorized wallet tests)
/// Returns Ok(error_message) if mint was correctly rejected
/// Panics if mint succeeded (security breach!)
pub async fn try_mint_expect_failure(
    client: &Client,
    base_url: &str,
    unauthorized_wallet: &Wallet,
    to_addr: &str,
    amount: &str,
    parents: Vec<String>,
) -> Result<String> {
    let output = TxOutput::new(to_addr.to_string(), amount.to_string(), None);
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Mint {
        outputs: vec![output],
    }));
    let mut block = Block::new(parents, payload, 0, None, compute_block_id).unwrap();

    // PoW
    mine_pow(&mut block);

    // Build WireBlock (signed by UNAUTHORIZED wallet)
    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json: Some(serde_json::to_string(&block.payload)?),
        nonce: block.nonce,
        network_id: NETWORK_ID.to_string(),
        protocol_version: PROTOCOL_VERSION as u16,
        signer_pk_hex: unauthorized_wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: None,
    };
    wb.signature_hex = unauthorized_wallet
        .sign(&canonical_wireblock_message(&wb))
        .unwrap();

    // Submit - EXPECT FAILURE
    let resp = client
        .post(format!("{}/submit/block", base_url))
        .json(&wb)
        .send()
        .await?;

    if resp.status().is_success() {
        panic!(
            "🚨 SECURITY BREACH: Unauthorized wallet {} successfully minted! This should NEVER happen.",
            &unauthorized_wallet.public_key_hex[..16]
        );
    }

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    // Verify it's specifically an UnauthorizedMint error
    if body.contains("UnauthorizedMint")
        || body.contains("unauthorized")
        || body.contains("mint security")
    {
        Ok(format!("✅ Mint correctly rejected: {} - {}", status, body))
    } else {
        Ok(format!(
            "⚠️ Mint rejected (unexpected reason): {} - {}",
            status, body
        ))
    }
}

/// Create and submit a Mint block
pub async fn mine_mint(
    client: &Client,
    base_url: &str,
    miner: &Wallet,
    to_addr: &str,
    amount: &str,
    parents: Vec<String>,
) -> Result<String> {
    let output = TxOutput::new(to_addr.to_string(), amount.to_string(), None);
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Mint {
        outputs: vec![output],
    }));
    let mut block = Block::new(parents, payload, 0, None, compute_block_id).unwrap();

    // PoW
    mine_pow(&mut block);

    // Build WireBlock
    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json: Some(serde_json::to_string(&block.payload)?),
        nonce: block.nonce,
        network_id: NETWORK_ID.to_string(),
        protocol_version: PROTOCOL_VERSION as u16,
        signer_pk_hex: miner.encoded_public_key(),
        signature_hex: String::new(),
        metadata: None,
    };
    wb.signature_hex = miner.sign(&canonical_wireblock_message(&wb)).unwrap();

    // Submit
    let resp = client
        .post(format!("{}/submit/block", base_url))
        .json(&wb)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Mint failed: {} - {}", status, body);
    }

    Ok(wb.id)
}

/// Send a transaction with fee split (Platform + Miner)
/// Returns (block_id, OutputRef for change)
pub async fn send_tx_with_split_fee(
    client: &Client,
    base_url: &str,
    sender: &Wallet,
    utxo_txid: &str,
    utxo_index: u32,
    to: &str,
    amount: &str,
    platform_addr: &str,
    total_fee: &str,
    change_addr: &str,
    change_amount: &str,
    parents: Vec<String>,
) -> Result<(String, OutputRef)> {
    // 1. Get tips if no parents provided
    let parents = if parents.is_empty() {
        let resp = client
            .post(format!("{}/v1/dag/tips", base_url))
            .json(&serde_json::json!({"limit": 2}))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to get tips: {} - {}", status, text);
        }
        resp.json::<Vec<String>>().await?
    } else {
        parents
    };

    // Calculate Split
    let total_fee_dec = rust_decimal::Decimal::from_str(total_fee).unwrap();
    let platform_ratio = rust_decimal::Decimal::from_str("0.45").unwrap();
    let platform_part = total_fee_dec * platform_ratio;
    // Miner part is what remains implicitly (input - output)
    // We must set tx.fee explicitly to this value for validation consistency
    let miner_part = total_fee_dec - platform_part;

    // 2. Build Transaction
    // Inputs must cover: Payment + Change + PlatformPart + MinerPart (implicit)
    // Actually, `change_amount` calculation from caller usually accounts for "total spent".
    // Caller: current - payment - total_fee.
    // Here: Outputs = Payment + Change + PlatformPart.
    // Inputs = Payment + Change + total_fee (if correctly calculated by caller).
    // Implicit Fee = Inputs - Outputs = (P + C + total) - (P + C + platform) = total - platform = miner_part.
    // Correct.

    let mut tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: utxo_txid.to_string(),
                index: utxo_index,
            },
        }],
        outputs: vec![
            TxOutput::new(to, amount, None), // idx 0 - payment
            TxOutput::new(change_addr, change_amount, None), // idx 1 - change
            TxOutput::new(platform_addr, platform_part.to_string(), None), // idx 2 - platform fee
            // STRICT VALIDATION: Explicitly pay miner fee to admin/coordinator
            // Caller `spam_transactions` passes `admin_address_ref` as `admin_addr`.
            TxOutput::new(platform_addr, miner_part.to_string(), None), // idx 3 - miner fee (explicit)
        ],
        fee: "0".into(), // Implicit fees forbidden. Set to 0.
        unlocks: vec![],
    };

    // Sign
    let msg = tx.signing_message(NETWORK_ID).unwrap();
    let sig = sender.sign(&msg).unwrap();
    tx.unlocks.push(Unlock {
        pubkey_hex: sender.encoded_public_key(),
        signature_b64: sig,
    });

    // 3. Build Block
    let payload = PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx));
    let mut block = Block::new(parents, Some(payload), 0, None, compute_block_id).unwrap();

    // PoW
    mine_pow(&mut block);

    // 4. Submit
    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json: Some(serde_json::to_string(&block.payload)?),
        nonce: block.nonce,
        network_id: NETWORK_ID.to_string(),
        protocol_version: PROTOCOL_VERSION as u16,
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
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Submit failed: {} - {}", status, body);
    }

    // Return block ID and change output ref
    let change_val = rust_decimal::Decimal::from_str(change_amount).unwrap_or_default();
    Ok((
        wb.id.clone(),
        OutputRef {
            txid: wb.id,
            index: 1, // Change is at index 1
            amount: change_val,
        },
    ))
}

/// Send a simple transaction (no explicit fee output)
/// Used for initial distribution where fee is not charged
/// Returns (block_id, change output ref index as string for compatibility)
pub async fn send_tx(
    client: &Client,
    base_url: &str,
    sender: &Wallet,
    utxo_txid: &str,
    utxo_index: u32,
    to: &str,
    amount: &str,
    change_addr: &str,
    change_amount: &str,
    parents: Vec<String>,
) -> Result<(String, String)> {
    // Build Transaction (no fee output)
    let mut tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: utxo_txid.to_string(),
                index: utxo_index,
            },
        }],
        outputs: vec![
            TxOutput::new(to, amount, None), // idx 0 - payment
            TxOutput::new(change_addr, change_amount, None), // idx 1 - change
        ],
        fee: "0".into(),
        unlocks: vec![],
    };

    // Sign
    let msg = tx.signing_message(NETWORK_ID).unwrap();
    let sig = sender.sign(&msg).unwrap();
    tx.unlocks.push(Unlock {
        pubkey_hex: sender.encoded_public_key(),
        signature_b64: sig,
    });

    // Build Block
    let payload = PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx));
    let mut block = Block::new(parents, Some(payload), 0, None, compute_block_id).unwrap();

    // PoW
    mine_pow(&mut block);

    // Submit
    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json: Some(serde_json::to_string(&block.payload)?),
        nonce: block.nonce,
        network_id: NETWORK_ID.to_string(),
        protocol_version: PROTOCOL_VERSION as u16,
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
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Submit failed: {} - {}", status, body);
    }

    Ok((wb.id, "".to_string()))
}

/// Spam transactions: chain multiple payments from sender to recipient
pub async fn spam_transactions(
    client: Client,
    base_url: String,
    sender: Wallet,
    start_txid: String,
    start_idx: u32,
    count: usize,
    payment: &str,
    admin_addr: &str,
    dest: String,
) -> Result<()> {
    let mut current_txid = start_txid;
    let mut current_idx = start_idx;
    let sender_addr = sender.get_address("8e");

    // Production fee ratio: 2.4% of payment
    let fee_amount = calculate_fee(payment);
    let payment_amount = rust_decimal::Decimal::from_str(payment).unwrap();

    // Track balance
    let mut current_amount = rust_decimal::Decimal::from_str("5000.0").unwrap();

    for i in 0..count {
        if current_amount < payment_amount + fee_amount {
            println!(
                "⚠️ Insufficient funds to continue spam ({} < {})",
                current_amount,
                payment_amount + fee_amount
            );
            break;
        }

        let change_val = current_amount - payment_amount - fee_amount;
        let change_str = change_val.to_string();

        let (new_tx, output_ref) = send_tx_with_split_fee(
            &client,
            &base_url,
            &sender,
            &current_txid,
            current_idx,
            &dest,
            payment,
            admin_addr,
            &fee_amount.to_string(), // This is total fee
            &sender_addr,
            &change_str,
            vec![],
        )
        .await?;

        // Update tracking
        current_txid = new_tx;
        current_idx = output_ref.index;
        current_amount = change_val;

        if i % 100 == 0 {
            println!(
                "   [{}] {}/{} txs... ID={} Idx={}",
                base_url, i, count, current_txid, current_idx
            );
        }
    }
    Ok(())
}

/// Get wallet balance from a node
pub async fn get_balance(
    client: &Client,
    base_url: &str,
    wallet: &Wallet,
) -> rust_decimal::Decimal {
    let addr = wallet.get_address("8e");
    let req = serde_json::json!({
        "bech32_addr": addr,
        "x25519_sk_hex": wallet.x25519_sk_hex().expect("No SK"),
        "ecdsa_pk_hex": wallet.encoded_public_key(),
        "scan_limit": 5000
    });

    let resp = match client
        .post(format!("{}/wallet/balance", base_url))
        .json(&req)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            println!("❌ Balance request failed: {}", e);
            return rust_decimal::Decimal::ZERO;
        }
    };

    if !resp.status().is_success() {
        return rust_decimal::Decimal::ZERO;
    }

    let body = resp.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let s = json["balance"].as_str().unwrap_or("0");
    rust_decimal::Decimal::from_str(s).unwrap_or_default()
}

/// Get wallet balance with UTXO details (for debugging)
pub async fn get_balance_with_utxos(
    client: &Client,
    base_url: &str,
    wallet: &Wallet,
) -> (rust_decimal::Decimal, serde_json::Value) {
    let addr = wallet.get_address("8e");
    let req = serde_json::json!({
        "bech32_addr": addr,
        "x25519_sk_hex": wallet.x25519_sk_hex().expect("No SK"),
        "ecdsa_pk_hex": wallet.encoded_public_key(),
        "scan_limit": 5000
    });

    let resp = client
        .post(format!("{}/wallet/balance", base_url))
        .json(&req)
        .send()
        .await
        .expect("Failed to send balance request");

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        panic!(
            "❌ Failed to get balance from {}: {} | Body: {}",
            base_url, status, body
        );
    }

    let body = resp.text().await.expect("Failed to read body");
    let json: serde_json::Value = serde_json::from_str(&body)
        .expect(&format!("Failed to parse JSON from {}: {}", base_url, body));
    let s = json["balance"].as_str().unwrap_or("0");
    (
        rust_decimal::Decimal::from_str(s).unwrap_or_default(),
        json["utxos"].clone(),
    )
}

/// Get block count from metrics endpoint
pub async fn get_block_count_metric(client: &Client, base_url: &str) -> i64 {
    let url = format!("{}/metrics", base_url);
    if let Ok(resp) = client.get(&url).send().await {
        if let Ok(text) = resp.text().await {
            for line in text.lines() {
                if line.starts_with("pms_blocks_total") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        return parts[1].parse::<i64>().unwrap_or(0);
                    }
                }
            }
        }
    }
    0
}
