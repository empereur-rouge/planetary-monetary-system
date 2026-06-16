//! Fee model = **BURNED at source** (phase 2b of the burn-at-source redesign).
//!
//! The PMS gas fee is `Σ(inputs) − Σ(outputs)`, destroyed at the transaction
//! level (conservation `out ≤ in`, phase 2a) — it is NEVER an output to an
//! admin/coordinator/shard address. These tests lock the new behavior on the
//! real `wallet_send_tx` handler:
//!   1. a burn-at-source transfer reduces total native supply by exactly the fee;
//!   2. underpaying the implicit fee (`in − out < expected`) is rejected.
//!
//! Run: `cargo test -p pms-server --test wallet_send_fees -- --nocapture`

use pms_testkit::{make_test_ctx_with_admin, mint_to_wallet_and_get_inputs, post_json};
use pms_token::fee::FeePolicy;
use pms_types::{Transaction, TxInput, TxOutput, Unlock};
use pms_wallet::{SignerBackend, Wallet};
use rust_decimal::Decimal;
use std::str::FromStr;

/// Clear env vars that could pollute the mint-authority list across tests.
fn clear_admin_env_conflicts() {
    unsafe {
        std::env::remove_var("PMS_ADMIN_PUBKEY");
    }
}

/// Build a context whose `node_wallet` (seed [7]) is authorised to mint, and
/// return it plus hrp + network_id.
async fn ctx_with_minter() -> (pms_testkit::TestCtx, String, String) {
    clear_admin_env_conflicts();
    let node = Wallet::from_seed(&[7u8; 32], None).unwrap();
    let node_pk = node.encoded_public_key();
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &node_pk);
    }
    let ctx = make_test_ctx_with_admin(vec![], vec![node_pk]).await.unwrap();
    let hrp = ctx.settings.address.hrp.clone();
    let nid = ctx.settings.network.network_id.clone();
    (ctx, hrp, nid)
}

/// Sign every input of `tx` with `w` (one unlock per input).
fn sign(w: &Wallet, mut tx: Transaction, nid: &str) -> Transaction {
    let msg = tx.signing_message(nid).expect("signing_message");
    let sig = w.sign(&msg).expect("sign");
    tx.unlocks = tx
        .inputs
        .iter()
        .map(|_| Unlock::new(w.public_key_hex.clone(), sig.clone()))
        .collect();
    tx
}

#[tokio::test]
async fn gas_fee_is_burned_reduces_total_supply() -> anyhow::Result<()> {
    let (ctx, hrp, nid) = ctx_with_minter().await;

    let alice = Wallet::from_seed(&[71u8; 32], None).unwrap();
    let bob = Wallet::from_seed(&[72u8; 32], None).unwrap();
    let bob_addr = bob.get_address(&hrp);
    let alice_addr = alice.get_address(&hrp);

    // Fund Alice with a single 100-PMS UTXO (official validated mint path).
    let (inputs, _minted) = mint_to_wallet_and_get_inputs(&ctx, &alice, "100").await?;
    let in_id = inputs[0].id.clone();
    let input_dec = Decimal::from_str("100")?;

    // Snapshot total native supply BEFORE the burn transfer.
    let (supply_before, _) = ctx.srv.adapter_arc().circulating_supply().await;

    // A→B with the fee BURNED: outputs = [B(taxable), A(change)],
    // change = in − taxable − fee → `in − out = fee` (no fee output).
    let taxable = Decimal::from_str("40")?;
    let fee_policy = FeePolicy::new(&ctx.settings.fees.base_fee, &ctx.settings.fees.ratio);
    let fee = fee_policy
        .compute_fee(&taxable.to_string())
        .expect("fee")
        .inner();
    let change = input_dec - taxable - fee;

    let tx = sign(
        &alice,
        Transaction {
            inputs: vec![TxInput {
                out: in_id.clone(),
            }],
            outputs: vec![
                TxOutput::new(bob_addr.clone(), taxable.to_string(), None),
                TxOutput::new(alice_addr.clone(), change.to_string(), None),
            ],
            fee: fee.to_string(),
            unlocks: vec![],
        },
        &nid,
    );

    let body = serde_json::json!({
        "tx": tx,
        "recipients_xpk": [alice.x25519_pub_hex, bob.x25519_pub_hex],
    });
    let (status, json) = post_json(&ctx.app, "/wallet/tx/send", body).await;
    println!("[FEE-BURN] send → {status} body={json}");
    assert!(
        status.is_success(),
        "burn-at-source transfer must be accepted: {json}"
    );

    let (supply_after, _) = ctx.srv.adapter_arc().circulating_supply().await;
    println!("[FEE-BURN] supply: before={supply_before} after={supply_after} (burned fee={fee})");
    assert_eq!(
        supply_after,
        supply_before - fee,
        "total native supply must DROP by exactly the burned gas fee"
    );
    Ok(())
}

#[tokio::test]
async fn underpaying_implicit_fee_is_rejected() -> anyhow::Result<()> {
    let (ctx, hrp, nid) = ctx_with_minter().await;

    let alice = Wallet::from_seed(&[73u8; 32], None).unwrap();
    let bob = Wallet::from_seed(&[74u8; 32], None).unwrap();
    let bob_addr = bob.get_address(&hrp);
    let alice_addr = alice.get_address(&hrp);

    let (inputs, _minted) = mint_to_wallet_and_get_inputs(&ctx, &alice, "100").await?;
    let in_id = inputs[0].id.clone();

    // Return the FULL input as recipient + change → implicit fee = 0 < expected.
    let tx = sign(
        &alice,
        Transaction {
            inputs: vec![TxInput {
                out: in_id.clone(),
            }],
            outputs: vec![
                TxOutput::new(bob_addr.clone(), "40".to_string(), None),
                TxOutput::new(alice_addr.clone(), "60".to_string(), None),
            ],
            fee: "0".to_string(),
            unlocks: vec![],
        },
        &nid,
    );

    let body = serde_json::json!({
        "tx": tx,
        "recipients_xpk": [alice.x25519_pub_hex, bob.x25519_pub_hex],
    });
    let (status, json) = post_json(&ctx.app, "/wallet/tx/send", body).await;
    println!("[FEE-BURN] underpay (burned=0) → {status} body={json}");
    assert!(
        !status.is_success(),
        "a transfer burning 0 fee must be rejected (insufficient fees), got {status}"
    );
    Ok(())
}
