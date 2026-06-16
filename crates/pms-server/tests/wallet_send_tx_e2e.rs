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

use pms_testkit::{make_test_ctx_with_admin, make_test_ctx_with_admin_sharded, sign_tx_inputs};
use pms_types::{OutputId, Transaction, TxInput, TxOutput, Unlock};
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
    println!("[E2E] FINAL BALANCES (fee burned at source)");
    println!("[E2E]   bob   = {bob_final}  (expected {amount})");
    println!("[E2E]   alice = {alice_final}  (expected {expected_alice})");
    println!("[E2E]   admin = {admin_final}  (expected 0 — fee is BURNED)");
    println!(
        "[E2E]   A+B   = {}  (expected {} = funded − burned fee)",
        alice_final + bob_final,
        faucet_amount - fee
    );
    println!("─────────────────────────────────────────────");

    assert_eq!(bob_final, amount, "Bob must receive exactly the sent amount");
    assert_eq!(
        alice_final, expected_alice,
        "Alice's change must equal funded − amount − fee"
    );
    // Fee model (phase 2b): the gas fee is BURNED at the source (in − out), not
    // paid to a coordinator/admin output. The admin receives nothing.
    assert_eq!(
        admin_final,
        Decimal::ZERO,
        "fee must be BURNED at source, not paid to admin"
    );
    assert_eq!(
        alice_final + bob_final,
        faucet_amount - fee,
        "supply: A + B == funded − fee (the gas fee was destroyed)"
    );
}

/// Same A→B flow but with COORDINATOR SHARDING enabled (`coord_shard_count > 0`).
///
/// Under the burn-at-source fee model (phase 2b) `prepare_tx` emits NO gas-fee
/// output, so nothing lands on any coordinator shard — the fee is burned
/// (`in − out`) regardless of the sharding config. This guards that enabling
/// sharding does not resurrect a fee output / break the transfer, and that the
/// fee is destroyed (supply: A + B == funded − fee).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_to_b_with_coordinator_sharding_fee_burned() {
    let settings0 = pms_config::load_config().expect("load config");
    let hrp = settings0.address.hrp.clone();

    let alice = Wallet::from_seed(&[33u8; 32], None).expect("alice");
    let bob = Wallet::from_seed(&[44u8; 32], None).expect("bob");
    let alice_addr = alice.get_address(&hrp);
    let bob_addr = bob.get_address(&hrp);

    // node_wallet == coordinator; authorize it as minter (faucet) as in dev tests.
    let node_wallet = Wallet::from_seed(&[7u8; 32], None).expect("node_wallet");
    let node_pk = node_wallet.encoded_public_key();
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &node_pk);
    }

    const SHARDS: u32 = 4;
    // Derive the SAME shard addresses the engine will use (deterministic from
    // node_wallet), so we can assert where the fee lands.
    let shard_wallets =
        pms_wallet::shard_derivation::derive_coord_shard_set(&node_wallet, SHARDS)
            .expect("derive coord shards");
    let shard_addrs: Vec<String> = shard_wallets.iter().map(|w| w.get_address(&hrp)).collect();
    println!("[E2E-shard] {} coordinator shard addresses derived", shard_addrs.len());

    // Sharding ON. No admin fee address needed — the fee goes to a shard.
    let ctx = make_test_ctx_with_admin_sharded(vec![], vec![node_pk], SHARDS)
        .await
        .expect("sharded ctx");
    let network_id = ctx.settings.network.network_id.clone();
    let app = ctx.app.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    sleep(Duration::from_millis(150)).await;
    println!("[E2E-shard] engine serving at {base}");

    let http = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();

    // Fund Alice.
    let faucet_amount = Decimal::from(100u32);
    let r = http
        .post(format!("{base}/admin/faucet"))
        .json(&json!({ "to": alice_addr, "amount": faucet_amount.to_string() }))
        .send()
        .await
        .unwrap();
    println!("[E2E-shard] faucet → {}", r.status());
    assert!(r.status().is_success(), "faucet must succeed");
    let funded = poll_balance(&http, &base, &alice_addr, faucet_amount).await;
    assert_eq!(funded, faucet_amount, "alice funded");

    // Prepare → with burn-at-source there is NO gas-fee output at all (the fee
    // is in − out). Confirm no output targets a shard address.
    let amount = Decimal::from(40u32);
    let prep_resp = http
        .post(format!("{base}/v1/tx/prepare"))
        .json(&json!({ "from": alice_addr, "to": bob_addr, "amount": amount.to_string() }))
        .send()
        .await
        .unwrap();
    let prep: serde_json::Value = prep_resp.json().await.unwrap();
    println!("[E2E-shard] prepare body={prep}");
    let fee = Decimal::from_str(prep["fee"].as_str().expect("fee")).expect("fee dec");

    let outputs = prep["unsigned_tx"]["outputs"].as_array().expect("outputs");
    let fee_to_shard = outputs.iter().any(|o| {
        let a = o["address"].as_str().unwrap_or("");
        shard_addrs.iter().any(|s| s.eq_ignore_ascii_case(a))
    });
    assert!(
        !fee_to_shard,
        "burn-at-source: prepare must NOT emit a gas-fee output to any shard"
    );
    println!("[E2E-shard] confirmed: no gas-fee output (fee burned), nothing to a shard");

    // Alice signs locally.
    let mut tx: Transaction =
        serde_json::from_value(prep["unsigned_tx"].clone()).expect("unsigned_tx");
    let msg = tx.signing_message(&network_id).expect("signing_message");
    let sig = alice.sign(&msg).expect("sign");
    tx.unlocks = tx
        .inputs
        .iter()
        .map(|_| Unlock::new(alice.public_key_hex.clone(), sig.clone()))
        .collect();

    // Submit via the real CLI helper.
    let alice_xpk = alice.x25519_pub_hex.clone();
    let (_h, bob_xpk) = decode_address(&bob_addr).expect("decode bob");
    let (status, block_id) =
        pms_utils::send_tx_http_to(&base, &tx, &[alice_xpk, bob_xpk], true)
            .await
            .expect("send_tx_http_to");
    println!("[E2E-shard] send_tx_http_to → {status} block={block_id:?}");
    assert_eq!(
        status,
        StatusCode::CREATED,
        "with sharding, the coordinator MUST accept the fee-to-shard tx \
         (regression guard for the shard-blind 'insufficient fees' bug)"
    );

    // Balances: Bob=40, Alice=100-40-fee, shards receive NOTHING (fee burned).
    let bob_final = poll_balance(&http, &base, &bob_addr, amount).await;
    let alice_final = get_balance(&http, &base, &alice_addr).await;
    let mut shard_total = Decimal::ZERO;
    for s in &shard_addrs {
        shard_total += get_balance(&http, &base, s).await;
    }
    let expected_alice = faucet_amount - amount - fee;
    println!("─────────────────────────────────────────────");
    println!("[E2E-shard] FINAL BALANCES (sharding on, fee burned)");
    println!("[E2E-shard]   bob          = {bob_final}  (expected {amount})");
    println!("[E2E-shard]   alice        = {alice_final}  (expected {expected_alice})");
    println!("[E2E-shard]   shards total = {shard_total}  (expected 0 — fee burned)");
    println!(
        "[E2E-shard]   A+B          = {}  (expected {} = funded − fee)",
        alice_final + bob_final,
        faucet_amount - fee
    );
    println!("─────────────────────────────────────────────");

    assert_eq!(bob_final, amount, "Bob must receive exactly the sent amount");
    assert_eq!(
        alice_final, expected_alice,
        "Alice's change must equal funded − amount − fee"
    );
    assert_eq!(
        shard_total,
        Decimal::ZERO,
        "burn-at-source: no fee lands on any coordinator shard"
    );
    assert_eq!(
        alice_final + bob_final,
        faucet_amount - fee,
        "supply: A + B == funded − fee (gas fee burned even with sharding on)"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// SECURITY: the encrypted `/wallet/tx/send` path must enforce the SAME validation
// as the plain hot-path (audit 2026-06, cause A). A client can craft a tx and
// POST it directly (bypassing `prepare_tx`), so these tests assert the bypasses
// are CLOSED: duplicate-input inflation, frozen sender/recipient, and time-locks.
// ════════════════════════════════════════════════════════════════════════════

/// Admin bearer token for the spawned engine (dev config reads it from
/// `env:PMS_ADMIN_TOKEN_DEV`). `admin_*` handlers do their own
/// `is_admin_authorized` header check (no loopback bypass), so admin requests
/// must carry it.
const TEST_ADMIN_TOKEN: &str = "audit-test-admin-token";

/// Spin up the real engine on an ephemeral port; node_wallet (seed [7]) is
/// authorised as minter so the faucet works. Returns (base_url, http, hrp, network_id).
async fn spawn_engine() -> (String, Client, String, String) {
    let node_wallet = Wallet::from_seed(&[7u8; 32], None).expect("node_wallet");
    let node_pk = node_wallet.encoded_public_key();
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &node_pk);
        std::env::set_var("PMS_ADMIN_TOKEN_DEV", TEST_ADMIN_TOKEN);
    }
    let ctx = make_test_ctx_with_admin(vec![], vec![node_pk])
        .await
        .expect("ctx");
    let hrp = ctx.settings.address.hrp.clone();
    let network_id = ctx.settings.network.network_id.clone();
    let app = ctx.app.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    sleep(Duration::from_millis(150)).await;
    let http = Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    (base, http, hrp, network_id)
}

/// Faucet `amount` PMS to `addr` (optionally time-locked); returns the faucet
/// block id, which is also the txid of the single minted UTXO (index 0).
async fn faucet(
    http: &Client,
    base: &str,
    addr: &str,
    amount: &str,
    locked_until: Option<u64>,
) -> String {
    let body = match locked_until {
        Some(t) => json!({ "to": addr, "amount": amount, "locked_until": t }),
        None => json!({ "to": addr, "amount": amount }),
    };
    let r = http
        .post(format!("{base}/admin/faucet"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "faucet must succeed: {}", r.status());
    let j: serde_json::Value = r.json().await.unwrap();
    j["block_id"].as_str().unwrap().to_string()
}

/// Submit a signed tx via the real CLI helper; returns the HTTP status.
async fn submit(base: &str, tx: &Transaction, xpks: &[String]) -> StatusCode {
    let (status, _id) = pms_utils::send_tx_http_to(base, tx, xpks, true)
        .await
        .expect("send_tx_http_to");
    status
}

/// **Inflation par input dupliqué** — une tx référençant le même UTXO deux fois,
/// payant 2× sa valeur, DOIT être rejetée (dédup d'inputs dans
/// `validate_transaction_full`). Avant le fix, le chemin chiffré la passait
/// (conservation comptait 2×A, le delta ne dépensait A qu'une fois → monnaie créée).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_input_cannot_inflate_via_encrypted_path() {
    let (base, http, hrp, network_id) = spawn_engine().await;
    let alice = Wallet::from_seed(&[51u8; 32], None).unwrap();
    let bob = Wallet::from_seed(&[52u8; 32], None).unwrap();
    let alice_addr = alice.get_address(&hrp);
    let bob_addr = bob.get_address(&hrp);

    let faucet_id = faucet(&http, &base, &alice_addr, "100", None).await;
    assert_eq!(
        poll_balance(&http, &base, &alice_addr, Decimal::from(100u32)).await,
        Decimal::from(100u32)
    );

    // Reference Alice's single 100-PMS UTXO TWICE, paying Bob 200.
    let utxo = OutputId {
        txid: faucet_id,
        index: 0,
    };
    let tx = Transaction {
        inputs: vec![
            TxInput { out: utxo.clone() },
            TxInput { out: utxo.clone() },
        ],
        outputs: vec![TxOutput::new(bob_addr.clone(), "200".to_string(), None)],
        fee: "0".to_string(),
        unlocks: vec![],
    };
    let tx = sign_tx_inputs(&alice, &tx, &network_id);
    let (_h, bob_xpk) = decode_address(&bob_addr).unwrap();

    let status = submit(&base, &tx, &[alice.x25519_pub_hex.clone(), bob_xpk]).await;
    println!("[DUP-INPUT] send → {status} (expect rejection)");
    assert!(
        !status.is_success(),
        "duplicate-input tx MUST be rejected (anti-inflation), got {status}"
    );

    sleep(Duration::from_millis(300)).await;
    let bob_bal = get_balance(&http, &base, &bob_addr).await;
    let alice_bal = get_balance(&http, &base, &alice_addr).await;
    println!("[DUP-INPUT] after: alice={alice_bal} (expect 100)  bob={bob_bal} (expect 0)");
    assert_eq!(bob_bal, Decimal::ZERO, "Bob must NOT receive inflated funds");
    assert_eq!(
        alice_bal,
        Decimal::from(100u32),
        "Alice's UTXO must be untouched (no spend, no inflation)"
    );
}

/// **Émetteur gelé** — une tx valide (préparée AVANT le gel) mais dont l'émetteur
/// est gelé DOIT être rejetée au POST `/wallet/tx/send` (gel non vérifié sur ce
/// chemin avant le fix).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frozen_sender_cannot_spend_via_encrypted_path() {
    let (base, http, hrp, network_id) = spawn_engine().await;
    let alice = Wallet::from_seed(&[53u8; 32], None).unwrap();
    let bob = Wallet::from_seed(&[54u8; 32], None).unwrap();
    let alice_addr = alice.get_address(&hrp);
    let bob_addr = bob.get_address(&hrp);

    faucet(&http, &base, &alice_addr, "100", None).await;
    assert_eq!(
        poll_balance(&http, &base, &alice_addr, Decimal::from(100u32)).await,
        Decimal::from(100u32)
    );

    // Prepare a VALID tx BEFORE freezing (prepare itself rejects frozen senders),
    // so the only reason a later send can fail is the freeze.
    let prep: serde_json::Value = http
        .post(format!("{base}/v1/tx/prepare"))
        .json(&json!({ "from": alice_addr, "to": bob_addr, "amount": "10" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut tx: Transaction = serde_json::from_value(prep["unsigned_tx"].clone()).unwrap();
    tx = sign_tx_inputs(&alice, &tx, &network_id);

    // Freeze Alice (loopback bypasses admin token in dev).
    let fr = http
        .post(format!("{base}/admin/compliance/freeze"))
        .header("Authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .json(&json!({ "address": alice_addr, "reason": "audit-test" }))
        .send()
        .await
        .unwrap();
    println!("[FROZEN-SENDER] freeze → {}", fr.status());
    assert!(fr.status().is_success(), "freeze must succeed");
    sleep(Duration::from_millis(300)).await;

    let (_h, bob_xpk) = decode_address(&bob_addr).unwrap();
    let status = submit(&base, &tx, &[alice.x25519_pub_hex.clone(), bob_xpk]).await;
    println!("[FROZEN-SENDER] send → {status} (expect rejection)");
    assert!(
        !status.is_success(),
        "frozen sender MUST NOT spend via /wallet/tx/send, got {status}"
    );
    assert_eq!(
        get_balance(&http, &base, &bob_addr).await,
        Decimal::ZERO,
        "Bob must not receive funds from a frozen sender"
    );
}

/// **Destinataire gelé** — une tx vers une adresse gelée DOIT être rejetée au POST.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frozen_recipient_rejected_via_encrypted_path() {
    let (base, http, hrp, network_id) = spawn_engine().await;
    let alice = Wallet::from_seed(&[55u8; 32], None).unwrap();
    let bob = Wallet::from_seed(&[56u8; 32], None).unwrap();
    let alice_addr = alice.get_address(&hrp);
    let bob_addr = bob.get_address(&hrp);

    faucet(&http, &base, &alice_addr, "100", None).await;
    assert_eq!(
        poll_balance(&http, &base, &alice_addr, Decimal::from(100u32)).await,
        Decimal::from(100u32)
    );

    // Prepare BEFORE freezing Bob (prepare rejects frozen recipients too).
    let prep: serde_json::Value = http
        .post(format!("{base}/v1/tx/prepare"))
        .json(&json!({ "from": alice_addr, "to": bob_addr, "amount": "10" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut tx: Transaction = serde_json::from_value(prep["unsigned_tx"].clone()).unwrap();
    tx = sign_tx_inputs(&alice, &tx, &network_id);

    let fr = http
        .post(format!("{base}/admin/compliance/freeze"))
        .header("Authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .json(&json!({ "address": bob_addr, "reason": "audit-test" }))
        .send()
        .await
        .unwrap();
    println!("[FROZEN-RECIPIENT] freeze(bob) → {}", fr.status());
    assert!(fr.status().is_success(), "freeze must succeed");
    sleep(Duration::from_millis(300)).await;

    let (_h, bob_xpk) = decode_address(&bob_addr).unwrap();
    let status = submit(&base, &tx, &[alice.x25519_pub_hex.clone(), bob_xpk]).await;
    println!("[FROZEN-RECIPIENT] send → {status} (expect rejection)");
    assert!(
        !status.is_success(),
        "tx to a frozen recipient MUST be rejected, got {status}"
    );
    assert_eq!(
        get_balance(&http, &base, &bob_addr).await,
        Decimal::ZERO,
        "frozen Bob must not receive funds"
    );
}

/// **Time-lock** — un UTXO `locked_until` dans le futur ne doit PAS être
/// dépensable via le chemin chiffré (check `check_input_time_locks`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timelocked_input_cannot_be_spent_early_via_encrypted_path() {
    let (base, http, hrp, network_id) = spawn_engine().await;
    let alice = Wallet::from_seed(&[57u8; 32], None).unwrap();
    let bob = Wallet::from_seed(&[58u8; 32], None).unwrap();
    let alice_addr = alice.get_address(&hrp);
    let bob_addr = bob.get_address(&hrp);

    // Faucet a UTXO locked 1h in the future.
    let locked_until = pms_utils::ts_ms() + 3_600_000;
    let faucet_id = faucet(&http, &base, &alice_addr, "100", Some(locked_until)).await;
    assert_eq!(
        poll_balance(&http, &base, &alice_addr, Decimal::from(100u32)).await,
        Decimal::from(100u32)
    );

    // Try to spend the locked UTXO now.
    let tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: faucet_id,
                index: 0,
            },
        }],
        outputs: vec![TxOutput::new(bob_addr.clone(), "100".to_string(), None)],
        fee: "0".to_string(),
        unlocks: vec![],
    };
    let tx = sign_tx_inputs(&alice, &tx, &network_id);
    let (_h, bob_xpk) = decode_address(&bob_addr).unwrap();

    let status = submit(&base, &tx, &[alice.x25519_pub_hex.clone(), bob_xpk]).await;
    println!("[TIMELOCK] send → {status} (expect rejection)");
    assert!(
        !status.is_success(),
        "time-locked UTXO MUST NOT be spendable early, got {status}"
    );
    assert_eq!(
        get_balance(&http, &base, &bob_addr).await,
        Decimal::ZERO,
        "Bob must not receive funds from a premature time-locked spend"
    );
}

/// **Double-dépense concurrente (TOCTOU)** — deux transferts signés par A,
/// dépensant le MÊME UTXO vers deux destinataires différents, soumis EN
/// PARALLÈLE. L'invariant : exactement UN est accepté, l'autre rejeté, et la
/// supply est conservée (pas d'inflation). Avant le guard de claim atomique,
/// les deux pouvaient passer la validation (lecture lock-free du UTXO set) puis
/// appliquer leur delta → A dépensé une fois mais B ET C crédités.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_double_spend_is_rejected() {
    let (base, http, hrp, network_id) = spawn_engine().await;
    let alice = Wallet::from_seed(&[61u8; 32], None).unwrap();
    let bob = Wallet::from_seed(&[62u8; 32], None).unwrap();
    let charlie = Wallet::from_seed(&[63u8; 32], None).unwrap();
    let alice_addr = alice.get_address(&hrp);
    let bob_addr = bob.get_address(&hrp);
    let charlie_addr = charlie.get_address(&hrp);

    let faucet_amount = Decimal::from(100u32);
    faucet(&http, &base, &alice_addr, "100", None).await;
    assert_eq!(
        poll_balance(&http, &base, &alice_addr, faucet_amount).await,
        faucet_amount
    );

    // Prepare two transfers that both select Alice's single 100-PMS UTXO.
    let amount = Decimal::from(40u32);
    let prep_b: serde_json::Value = http
        .post(format!("{base}/v1/tx/prepare"))
        .json(&json!({ "from": alice_addr, "to": bob_addr, "amount": amount.to_string() }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let prep_c: serde_json::Value = http
        .post(format!("{base}/v1/tx/prepare"))
        .json(&json!({ "from": alice_addr, "to": charlie_addr, "amount": amount.to_string() }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fee = Decimal::from_str(prep_b["fee"].as_str().expect("fee")).expect("fee dec");

    let tx_b = sign_tx_inputs(
        &alice,
        &serde_json::from_value(prep_b["unsigned_tx"].clone()).unwrap(),
        &network_id,
    );
    let tx_c = sign_tx_inputs(
        &alice,
        &serde_json::from_value(prep_c["unsigned_tx"].clone()).unwrap(),
        &network_id,
    );

    let (_h, bob_xpk) = decode_address(&bob_addr).unwrap();
    let (_h, charlie_xpk) = decode_address(&charlie_addr).unwrap();
    let axpk = alice.x25519_pub_hex.clone();

    // Fire BOTH concurrently — they race on Alice's single UTXO.
    let xpks_b = [axpk.clone(), bob_xpk];
    let xpks_c = [axpk, charlie_xpk];
    let (rb, rc) = tokio::join!(
        pms_utils::send_tx_http_to(&base, &tx_b, &xpks_b, true),
        pms_utils::send_tx_http_to(&base, &tx_c, &xpks_c, true),
    );
    let status_b = rb.unwrap().0;
    let status_c = rc.unwrap().0;
    println!("[CONCURRENT-DS] B → {status_b}   C → {status_c}");

    let created = [status_b, status_c]
        .iter()
        .filter(|s| **s == StatusCode::CREATED)
        .count();
    assert_eq!(
        created, 1,
        "exactly ONE of two concurrent same-UTXO spends may be accepted (got {created})"
    );

    // No inflation: only one recipient is paid; Alice spent her UTXO once.
    // The winner's transfer applies async — poll until Alice's change settles.
    let expected_alice = faucet_amount - amount - fee;
    poll_balance(&http, &base, &alice_addr, expected_alice).await;
    let bob_bal = get_balance(&http, &base, &bob_addr).await;
    let charlie_bal = get_balance(&http, &base, &charlie_addr).await;
    let alice_bal = get_balance(&http, &base, &alice_addr).await;
    println!(
        "[CONCURRENT-DS] FINAL: alice={alice_bal} bob={bob_bal} charlie={charlie_bal} (fee={fee})"
    );
    assert_eq!(
        bob_bal + charlie_bal,
        amount,
        "exactly ONE transfer of {amount} may land (no inflation: B+C must == amount)"
    );
    assert_eq!(
        alice_bal, expected_alice,
        "Alice's UTXO must be spent EXACTLY once (single change output)"
    );
}
