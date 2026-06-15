//! End-to-end test of the **non-custodial A→B transfer through the coordinator**.
//!
//! This is the path `tools-cli tx` now uses (fixed): user A signs the
//! transaction **locally** with her own key, then the *already-signed* tx is
//! POSTed to the coordinator via the **real** [`pms_utils::send_tx_http_to`]
//! helper — the exact code path the CLI invokes. The coordinator verifies A's
//! per-input signatures, encrypts the payload, and wraps the tx into a
//! **coordinator-signed** block (A is NOT in the coordinator key set, so a
//! self-signed block on `/submit/block` would be rejected by `single_writer_gate`
//! on testnet/mainnet — that is the bug this fix closes).
//!
//! The test serves the real engine router on an ephemeral TCP port so the
//! reqwest-based helper is exercised over genuine HTTP (not an in-process
//! `oneshot`). It then asserts the **funds actually move** A→B with full value
//! conservation (A − amount − fee = change, B = amount, admin = fee).
//!
//! Run:
//! ```bash
//! cargo test -p pms-server --test wallet_send_tx_e2e -- --nocapture
//! ```

use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use pms_testkit::make_test_ctx_with_admin;
use pms_types::{Transaction, Unlock};
use pms_wallet::{SignerBackend, Wallet, decode_address};
use reqwest::{Client, StatusCode};
use rust_decimal::Decimal;
use serde_json::json;
use tokio::time::sleep;

/// Read the native-PMS balance of `addr` via `POST /v1/balance`.
async fn get_balance(http: &Client, base: &str, addr: &str) -> Decimal {
    let resp = http
        .post(format!("{base}/v1/balance"))
        .json(&json!({ "address": addr }))
        .send()
        .await
        .expect("balance request");
    let body: serde_json::Value = resp.json().await.expect("balance json");
    let s = body["balance"].as_str().unwrap_or("0");
    Decimal::from_str(s).unwrap_or(Decimal::ZERO)
}

/// Poll `addr`'s balance until it equals `want` (persistence is async) or a
/// ~6s budget elapses; returns the last observed value either way so the
/// caller's assertion prints the real number on mismatch.
async fn poll_balance(http: &Client, base: &str, addr: &str, want: Decimal) -> Decimal {
    let mut last = Decimal::ZERO;
    for _ in 0..60 {
        last = get_balance(http, base, addr).await;
        if last == want {
            return last;
        }
        sleep(Duration::from_millis(100)).await;
    }
    last
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_to_b_through_coordinator_via_send_tx_http() {
    // ── 1. Config + addresses (need hrp before building the ctx so the admin
    //       fee-recipient address can be registered) ────────────────────────
    let settings0 = pms_config::load_config().expect("load config");
    let hrp = settings0.address.hrp.clone();

    let alice = Wallet::from_seed(&[11u8; 32], None).expect("alice wallet");
    let bob = Wallet::from_seed(&[22u8; 32], None).expect("bob wallet");
    let admin = Wallet::from_seed(&[9u8; 32], None).expect("admin wallet");

    let alice_addr = alice.get_address(&hrp);
    let bob_addr = bob.get_address(&hrp);
    let admin_addr = admin.get_address(&hrp);

    println!("[E2E] hrp={hrp}");
    println!("[E2E] alice = {alice_addr}");
    println!("[E2E] bob   = {bob_addr}");
    println!("[E2E] admin = {admin_addr} (fee recipient)");

    // ── 2. Build the engine (dev mode → coordinator/node_wallet block sig
    //       accepted). Register `admin` as the fee-recipient so prepare_tx has
    //       a home for the gas fee output, and authorize the ctx's node_wallet
    //       (deterministic seed [7]) as a minter so the faucet can fund Alice. ─
    let node_wallet = Wallet::from_seed(&[7u8; 32], None).expect("ctx node_wallet");
    let node_pk = node_wallet.encoded_public_key();
    // Established test convention (policy.rs:28): in dev mode `PMS_TEST_ADMIN_PUBKEY`
    // overrides the mint-authority list so the ctx's node_wallet may mint (faucet).
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &node_pk);
    }
    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![node_pk])
        .await
        .expect("make_test_ctx_with_admin");
    let network_id = ctx.settings.network.network_id.clone();
    let app = ctx.app.clone();
    println!("[E2E] network_id={network_id}");

    // ── 3. Serve the REAL router on an ephemeral port so reqwest (and thus the
    //       real `send_tx_http_to`) talks genuine HTTP. ─────────────────────
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });
    sleep(Duration::from_millis(150)).await;
    println!("[E2E] engine serving at {base}");

    let http = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .expect("http client");

    // ── 4. Fund Alice via faucet (loopback bypasses the admin token in dev) ─
    let faucet_amount = Decimal::from(100u32);
    let r = http
        .post(format!("{base}/admin/faucet"))
        .json(&json!({ "to": alice_addr, "amount": faucet_amount.to_string() }))
        .send()
        .await
        .expect("faucet request");
    let faucet_status = r.status();
    let faucet_body: serde_json::Value = r.json().await.unwrap_or(json!({}));
    println!("[E2E] faucet → {faucet_status} body={faucet_body}");
    assert!(
        faucet_status.is_success(),
        "faucet must succeed (got {faucet_status})"
    );

    let alice_funded = poll_balance(&http, &base, &alice_addr, faucet_amount).await;
    println!("[E2E] alice balance after faucet = {alice_funded}");
    assert_eq!(
        alice_funded, faucet_amount,
        "alice must hold exactly {faucet_amount} PMS after faucet"
    );

    // ── 5. Prepare A→B (server-side coin selection → unsigned tx + tx_hash) ─
    let amount = Decimal::from(40u32);
    let prep_resp = http
        .post(format!("{base}/v1/tx/prepare"))
        .json(&json!({ "from": alice_addr, "to": bob_addr, "amount": amount.to_string() }))
        .send()
        .await
        .expect("prepare request");
    let prep_status = prep_resp.status();
    let prep: serde_json::Value = prep_resp.json().await.expect("prepare json");
    println!("[E2E] prepare → {prep_status} body={prep}");
    assert_eq!(prep_status, StatusCode::OK, "prepare must succeed");

    let fee = Decimal::from_str(prep["fee"].as_str().expect("fee field")).expect("fee decimal");
    let transfer_fee =
        Decimal::from_str(prep["transfer_fee"].as_str().expect("transfer_fee field"))
            .expect("transfer_fee decimal");
    println!("[E2E] gas fee={fee}  transfer_fee={transfer_fee}");
    assert_eq!(
        transfer_fee,
        Decimal::ZERO,
        "no transfer contract registered → transfer_fee must be 0"
    );

    let mut tx: Transaction =
        serde_json::from_value(prep["unsigned_tx"].clone()).expect("deserialize unsigned_tx");
    let tx_hash = prep["tx_hash"].as_str().expect("tx_hash").to_string();

    // ── 6. Alice signs LOCALLY. Her private key never leaves this process —
    //       only the resulting signature/pubkey go into the unlocks. We also
    //       assert the server's tx_hash equals the canonical signing message. ─
    let msg = tx
        .signing_message(&network_id)
        .expect("recompute signing_message");
    assert_eq!(
        msg, tx_hash,
        "prepare tx_hash must equal Transaction::signing_message(network_id)"
    );
    let sig_b64 = alice.sign(&msg).expect("alice signs");
    tx.unlocks = tx
        .inputs
        .iter()
        .map(|_| Unlock::new(alice.public_key_hex.clone(), sig_b64.clone()))
        .collect();
    println!(
        "[E2E] alice signed tx: inputs={} outputs={} unlocks={}",
        tx.inputs.len(),
        tx.outputs.len(),
        tx.unlocks.len()
    );

    // ── 7. Submit via the REAL CLI helper (`pms_utils::send_tx_http_to`).
    //       recipients_xpk = [sender, dest] — exactly what the CLI builds. ───
    let alice_xpk = alice.x25519_pub_hex.clone();
    let (_bh20, bob_xpk) = decode_address(&bob_addr).expect("decode bob addr");
    let recipients_xpk = vec![alice_xpk, bob_xpk];

    let (status, block_id) = pms_utils::send_tx_http_to(&base, &tx, &recipients_xpk, true)
        .await
        .expect("send_tx_http_to");
    println!("[E2E] send_tx_http_to → {status} block_id={block_id:?}");
    assert_eq!(
        status,
        StatusCode::CREATED,
        "coordinator must accept A's signed tx and wrap it in a block"
    );
    assert!(block_id.is_some(), "coordinator must return the new block id");

    // ── 8. Assert the funds moved, with full conservation. ─────────────────
    let bob_final = poll_balance(&http, &base, &bob_addr, amount).await;
    let alice_final = get_balance(&http, &base, &alice_addr).await;
    let admin_final = get_balance(&http, &base, &admin_addr).await;

    let expected_alice = faucet_amount - amount - fee; // change output back to A
    println!("─────────────────────────────────────────────");
    println!("[E2E] FINAL BALANCES");
    println!("[E2E]   bob   = {bob_final}  (expected {amount})");
    println!("[E2E]   alice = {alice_final}  (expected {expected_alice})");
    println!("[E2E]   admin = {admin_final}  (expected {fee})");
    println!(
        "[E2E]   sum   = {}  (expected {faucet_amount})",
        alice_final + bob_final + admin_final
    );
    println!("─────────────────────────────────────────────");

    assert_eq!(bob_final, amount, "Bob must receive exactly the sent amount");
    assert_eq!(
        alice_final, expected_alice,
        "Alice's change must equal funded − amount − fee"
    );
    assert_eq!(admin_final, fee, "fee must land on the admin fee-recipient");
    assert_eq!(
        alice_final + bob_final + admin_final,
        faucet_amount,
        "value conservation: nothing created or destroyed"
    );
}
