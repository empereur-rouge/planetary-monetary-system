use pms_testkit::{
    forge_signed_wire_block_for_test, get_json, make_test_ctx, make_test_ctx_with_admin,
    mint_to_wallet_and_get_inputs, post_json, post_json_admin, sign_tx_inputs,
};
use pms_types::{OutputId, PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput};
use pms_types_nft::{NftAction, NftMetadata};
use pms_types_payload::TokenMetadata;
use pms_storage::DagStorage;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;

// ═══════════════════════════════════════════════════════════════════════════════
// Shared helpers
// ═══════════════════════════════════════════════════════════════════════════════

fn print_activity(label: &str, addr: &str, json: &serde_json::Value) {
    println!("\n=== [{label}] Activity for {addr} ===");
    println!("  Full response: {json:#}");
    if let Some(items) = json["items"].as_array() {
        println!("  Item count: {}", items.len());
        for (i, item) in items.iter().enumerate() {
            println!(
                "  [{i}] type={}, dir={}, amount={}, asset_id={}, counterparty={}, block_id={}",
                item["activity_type"],
                item["direction"],
                item["amount"],
                item["asset_id"],
                item["counterparty"],
                item["block_id"]
            );
        }
    }
}

fn print_api(label: &str, status: http::StatusCode, json: &serde_json::Value) {
    println!("\n=== [{label}] API Response ===");
    println!("  Status: {status}");
    println!("  Body: {json:#}");
}

async fn get_tips(ctx: &pms_testkit::TestCtx) -> Vec<String> {
    let mut parents = ctx.store.top_tips(2).await.unwrap_or_default();
    if parents.is_empty() {
        let ids = ctx.store.all_block_ids().await.unwrap_or_default();
        if let Some(g) = ids.first() {
            parents = vec![g.clone()];
        }
    }
    parents.sort();
    parents.dedup();
    parents
}

fn clear_admin_env_conflicts() {
    unsafe {
        for k in [
            "PMS__ADMIN__SIGNER_PUBKEYS",
            "PMS__ADMIN__SIGNER_PUBKEYS__0",
            "PMS__ADMIN__SIGNER_PUBKEYS__1",
            "PMS__ADMIN__SIGNER_PUBKEYS__2",
            "PMS__ADMIN",
        ] {
            std::env::remove_var(k);
        }
    }
}

/// Token admin HTTP (v0.9.3 fix). Les endpoints `/admin/*` re-vérifient
/// `is_admin_authorized` DANS le handler (pas seulement le middleware), donc
/// les appels admin doivent (a) configurer ce token via `PMS_ADMIN_TOKEN_DEV`
/// AVANT make_test_ctx, et (b) l'envoyer via `post_json_admin`. Avant ce fix,
/// `activity_seize_*` / freeze / unfreeze / reverse POSTaient sans token → 401.
const ADMIN_TOKEN: &str = "activity-test-admin-token";

fn set_admin_token_env() {
    unsafe {
        std::env::set_var("PMS_ADMIN_TOKEN_DEV", ADMIN_TOKEN);
    }
}

// `sign_tx_inputs` est maintenant le helper partagé `pms_testkit::sign_tx_inputs`
// (importé en tête). Tout test forgeant une TxUtxo l'utilise.

fn setup_admin_ctx() -> (std::sync::Arc<Wallet>, String, String) {
    clear_admin_env_conflicts();
    set_admin_token_env();
    let admin = std::sync::Arc::new(Wallet::from_seed(&[7u8; 32], None).unwrap());
    let hrp = "8e";
    let admin_addr = admin.get_address(hrp);
    let admin_pubkey = admin.encoded_public_key();
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &admin_pubkey);
    }
    (admin, admin_addr, admin_pubkey)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 1: Mint appears in activity
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_mint_appears() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let wallet = Wallet::from_seed(&[42u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    let (inputs, minted) = mint_to_wallet_and_get_inputs(&ctx, &wallet, "10.00").await?;
    println!("\n=== [MINT] Setup ===");
    println!("  Minted {} to {}", minted, addr);
    println!("  Input UTXO: txid={}, index={}", inputs[0].id.txid, inputs[0].id.index);

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let path = format!("/v1/wallet/{}/activity", addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_api("MINT query", status, &json);
    print_activity("MINT", &addr, &json);

    assert!(status.is_success(), "activity query failed: {status} body={json}");

    let items = json["items"].as_array().expect("items should be an array");
    assert!(!items.is_empty(), "expected at least 1 activity item, got 0");

    let mint_item = items.iter().find(|i| i["activity_type"] == "mint");
    assert!(mint_item.is_some(), "expected a 'mint' activity item, items={json}");

    let mint = mint_item.unwrap();
    assert_eq!(mint["direction"], "in");
    assert_eq!(mint["amount"], "10.00");

    println!("  [OK] mint: direction={}, amount={}", mint["direction"], mint["amount"]);
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 2: Encrypted transfer via /wallet/tx/send appears as transfer_in
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_transfer_in_encrypted() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let w_from = Wallet::from_seed(&[10u8; 32], None).unwrap();
    let w_to = Wallet::from_seed(&[11u8; 32], None).unwrap();
    let from_addr = w_from.get_address(hrp);
    let to_addr = w_to.get_address(hrp);

    println!("\n=== [TRANSFER_IN_ENCRYPTED] Setup ===");
    println!("  Sender: {from_addr}");
    println!("  Receiver: {to_addr}");

    let (inputs, _) = mint_to_wallet_and_get_inputs(&ctx, &w_from, "5.00").await?;
    let u = inputs.first().expect("need at least one input from mint");
    println!("  Minted 5.00 to sender, UTXO: {}#{}", u.id.txid, u.id.index);

    // Force manual UTXO persistence for the mint
    {
        let ua = pms_storage::rocks_store::utxo::UtxoApply {
            txid: u.id.txid.clone(),
            inputs: vec![],
            outputs: vec![(from_addr.clone(), u.amount.clone(), None)],
        };
        ctx.store.utxo_apply_tx_atomic(&ua).await?;
    }

    let taxable_amount = "4.00";
    let fee_policy =
        pms_token::fee::FeePolicy::new(&ctx.settings.fees.base_fee, &ctx.settings.fees.ratio);
    let fee_dec = fee_policy.compute_fee(taxable_amount).expect("fee computation");
    let fee = fee_dec.to_string();

    let input_dec: rust_decimal::Decimal = u.amount.parse().unwrap();
    let taxable_dec: rust_decimal::Decimal = taxable_amount.parse().unwrap();
    let change_dec = input_dec - taxable_dec - fee_dec.inner();
    let change = change_dec.normalize().to_string();
    println!("  Fee: {fee}, Change: {change}");

    // Build + sign the tx client-side (the handler does NOT sign; v0.9.0
    // canonical validation requires valid unlocks from the sender wallet).
    let tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: u.id.txid.clone(),
                index: u.id.index,
            },
        }],
        outputs: vec![
            TxOutput::new(to_addr.clone(), taxable_amount.to_string(), None),
            TxOutput::new(admin_addr.clone(), fee.clone(), None),
            TxOutput::new(from_addr.clone(), change.clone(), None),
        ],
        fee: fee.clone(),
        unlocks: vec![],
    };
    let signed = sign_tx_inputs(&w_from, &tx, &ctx.settings.network.network_id);
    let body = serde_json::json!({
        "tx": serde_json::to_value(&signed).unwrap(),
        "recipients_xpk": [ w_to.x25519_pub_hex.clone() ]
    });
    let (status, json) = post_json(&ctx.app, "/wallet/tx/send", body).await;
    print_api("SEND", status, &json);
    assert!(status.is_success(), "send failed: {status} body={json}");

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Receiver should see transfer_in via encrypted payload decryption
    let path = format!(
        "/v1/wallet/{}/activity?x25519_sk_hex={}",
        to_addr,
        w_to.x25519_sk_hex().unwrap()
    );
    let (status, json) = get_json(&ctx.app, &path).await;
    print_api("TRANSFER_IN query", status, &json);
    print_activity("TRANSFER_IN_ENCRYPTED", &to_addr, &json);

    assert!(status.is_success(), "receiver activity failed: {status} body={json}");

    let items = json["items"].as_array().expect("items should be an array");
    let transfer_in = items.iter().find(|i| i["activity_type"] == "transfer_in");
    assert!(transfer_in.is_some(), "expected 'transfer_in' for receiver, items={json}");

    let ti = transfer_in.unwrap();
    assert_eq!(ti["direction"], "in");
    println!("  [OK] transfer_in: direction={}, amount={}", ti["direction"], ti["amount"]);

    // ── Now test with type filter (the path used by Heshima Network server) ──
    println!("\n=== [TRANSFER_IN_ENCRYPTED] With type=transfer_in filter ===");
    let path_typed = format!(
        "/v1/wallet/{}/activity?x25519_sk_hex={}&type=transfer_in,transfer_out,transfer_self",
        to_addr,
        w_to.x25519_sk_hex().unwrap()
    );
    let (status2, json2) = get_json(&ctx.app, &path_typed).await;
    print_api("TRANSFER_IN typed query", status2, &json2);
    print_activity("TRANSFER_IN_ENCRYPTED (typed)", &to_addr, &json2);
    assert!(status2.is_success(), "typed activity failed: {status2} body={json2}");

    let items2 = json2["items"].as_array().expect("items should be an array");
    println!("  Typed items count: {}", items2.len());
    for (i, item) in items2.iter().enumerate() {
        println!("  [{i}] type={}, dir={}, amount={}", item["activity_type"], item["direction"], item["amount"]);
    }
    let transfer_in2 = items2.iter().find(|i| i["activity_type"] == "transfer_in");
    assert!(
        transfer_in2.is_some(),
        "expected 'transfer_in' with type filter for receiver, got items={json2}"
    );
    println!("  [OK] transfer_in found with type filter");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 3: Plain transfer with change (sender+receiver perspectives)
//
// NOTE: This test uses the plain persist path (persist_block), NOT the HTTP
// handler.  In the plain path, resolve_sender() tries to look up the input
// UTXO after it has been spent → returns None → sender classified as
// transfer_in (for their change output).  This is a known limitation of the
// plain/P2P persist path.
//
// The HTTP handler (POST /wallet/tx/send) resolves the sender BEFORE
// spending the UTXOs, so encrypted blocks get correct transfer_out
// classification.  See activity_transfer_out_via_precompute test.
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_transfer_with_change() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();
    let meta = WireMeta::from(&ctx.settings);

    let sender = Wallet::from_seed(&[100u8; 32], None).unwrap();
    let receiver = Wallet::from_seed(&[101u8; 32], None).unwrap();
    let sender_addr = sender.get_address(hrp);
    let receiver_addr = receiver.get_address(hrp);

    println!("\n=== [TRANSFER_WITH_CHANGE] Setup ===");
    println!("  Sender: {sender_addr}");
    println!("  Receiver: {receiver_addr}");

    // Mint 10.00 to sender
    let (inputs, _) = mint_to_wallet_and_get_inputs(&ctx, &sender, "10.00").await?;
    let u = &inputs[0];
    println!("  Minted 10.00, UTXO: {}#{}", u.id.txid, u.id.index);

    // Force UTXO persistence
    {
        let ua = pms_storage::rocks_store::utxo::UtxoApply {
            txid: u.id.txid.clone(),
            inputs: vec![],
            outputs: vec![(sender_addr.clone(), u.amount.clone(), None)],
        };
        ctx.store.utxo_apply_tx_atomic(&ua).await?;
    }

    // Build TxUtxo: sender → receiver (5.00) + change to sender (4.50) + fee to admin (0.50)
    // NOTE: sum(outputs) must == sum(inputs), the fee field is metadata only
    let tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: u.id.txid.clone(),
                index: u.id.index,
            },
        }],
        outputs: vec![
            TxOutput::new(receiver_addr.clone(), "5.00".to_string(), None),
            TxOutput::new(sender_addr.clone(), "4.50".to_string(), None),
            TxOutput::new(admin_addr.clone(), "0.50".to_string(), None),
        ],
        fee: "0.50".to_string(),
        unlocks: vec![],
    };

    let tips = get_tips(&ctx).await;
    let wb = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        2,
        Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(sign_tx_inputs(
            &sender,
            &tx,
            &meta.network_id,
        )))),
    );
    println!("  Forged TxUtxo block: {}", wb.id);

    let res = ctx.srv.adapter_arc().persist_block(&wb).await?;
    println!("  Persist result: {res:?}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Sender perspective (plain persist path): input UTXOs are spent during
    // persist, so resolve_sender() returns None → sender sees transfer_in for
    // their change output.  In the encrypted handler path, sender is resolved
    // BEFORE UTXO spending, giving correct transfer_out.
    let path = format!("/v1/wallet/{}/activity", sender_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_api("TRANSFER sender query", status, &json);
    print_activity("TRANSFER_SENDER", &sender_addr, &json);

    assert!(status.is_success(), "sender activity failed: {status} body={json}");
    let items = json["items"].as_array().expect("items should be an array");

    // Plain path: resolve_sender returns None → sender sees transfer_in (change output)
    let has_transfer = items.iter().any(|i| {
        let at = i["activity_type"].as_str().unwrap_or("");
        at == "transfer_in" || at == "transfer_out"
    });
    assert!(
        has_transfer,
        "expected sender to see transfer activity for TxUtxo block, items={json}"
    );
    let tx_item = items.iter().find(|i| {
        let at = i["activity_type"].as_str().unwrap_or("");
        at == "transfer_in" || at == "transfer_out"
    }).unwrap();
    println!(
        "  [OK] sender sees: type={}, dir={}, amount={} (plain path: resolve_sender=None → transfer_in)",
        tx_item["activity_type"], tx_item["direction"], tx_item["amount"]
    );

    // Receiver should see transfer_in
    let path = format!("/v1/wallet/{}/activity", receiver_addr);
    let (_status, json) = get_json(&ctx.app, &path).await;
    print_activity("TRANSFER_IN (receiver)", &receiver_addr, &json);

    let items = json["items"].as_array().expect("items");
    let transfer_in = items.iter().find(|i| i["activity_type"] == "transfer_in");
    assert!(
        transfer_in.is_some(),
        "expected 'transfer_in' for receiver, items={json}"
    );

    let ti = transfer_in.unwrap();
    assert_eq!(ti["direction"], "in");
    assert_eq!(ti["amount"], "5.00");
    println!(
        "  [OK] receiver: transfer_in, direction={}, amount={}",
        ti["direction"], ti["amount"]
    );

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 4: Transfer self (consolidation: sender == receiver in outputs)
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_transfer_self() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();
    let meta = WireMeta::from(&ctx.settings);

    let wallet = Wallet::from_seed(&[110u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    println!("\n=== [TRANSFER_SELF] Setup ===");
    println!("  Wallet: {addr}");

    let (inputs, _) = mint_to_wallet_and_get_inputs(&ctx, &wallet, "10.00").await?;
    let u = &inputs[0];
    println!("  Minted 10.00, UTXO: {}#{}", u.id.txid, u.id.index);

    // Force UTXO persistence
    {
        let ua = pms_storage::rocks_store::utxo::UtxoApply {
            txid: u.id.txid.clone(),
            inputs: vec![],
            outputs: vec![(addr.clone(), u.amount.clone(), None)],
        };
        ctx.store.utxo_apply_tx_atomic(&ua).await?;
    }

    // Build TxUtxo where sender == receiver (consolidation)
    // NOTE: sum(outputs) must == sum(inputs), fee goes to admin as output
    let admin_addr = ctx.node_wallet.get_address(hrp);
    let tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: u.id.txid.clone(),
                index: u.id.index,
            },
        }],
        outputs: vec![
            TxOutput::new(addr.clone(), "9.50".to_string(), None),
            TxOutput::new(admin_addr.clone(), "0.50".to_string(), None),
        ],
        fee: "0.50".to_string(),
        unlocks: vec![],
    };

    let tips = get_tips(&ctx).await;
    let wb = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        2,
        Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(sign_tx_inputs(
            &wallet,
            &tx,
            &meta.network_id,
        )))),
    );
    println!("  Forged self-transfer block: {}", wb.id);

    let res = ctx.srv.adapter_arc().persist_block(&wb).await?;
    println!("  Persist result: {res:?}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let path = format!("/v1/wallet/{}/activity", addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_api("TRANSFER_SELF query", status, &json);
    print_activity("TRANSFER_SELF", &addr, &json);

    assert!(status.is_success(), "activity query failed: {status} body={json}");
    let items = json["items"].as_array().expect("items should be an array");

    // Plain path: resolve_sender returns None → sender sees transfer_in.
    // NOTE: This "consolidation" test has an admin fee output, so even with
    // resolved sender it would be transfer_out (not transfer_self) because
    // has_other_recipients=true.  A true transfer_self requires ALL outputs
    // to the sender (no other recipients).
    let has_transfer = items.iter().any(|i| {
        let at = i["activity_type"].as_str().unwrap_or("");
        at == "transfer_in" || at == "transfer_out"
    });
    assert!(has_transfer, "expected transfer activity for consolidation, items={json}");

    let tx_item = items.iter().find(|i| {
        let at = i["activity_type"].as_str().unwrap_or("");
        at == "transfer_in" || at == "transfer_out"
    }).unwrap();
    println!(
        "  [OK] consolidation: type={}, direction={}, amount={} (plain path: resolve_sender=None)",
        tx_item["activity_type"], tx_item["direction"], tx_item["amount"]
    );

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 4b: Pre-computed transfer_out via sender resolution BEFORE UTXO spend
//
// Verifies that classify_for_storage() and precompute_all_items() correctly
// produce transfer_out for the sender when sender_addr is pre-resolved.
// This is the logic used by the encrypted handler (POST /wallet/tx/send)
// which resolves the sender BEFORE spending UTXOs.
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_transfer_out_via_precompute() -> anyhow::Result<()> {
    let sender_addr = "sender_abc";
    let receiver_addr = "receiver_xyz";
    let admin_addr = "admin_fee";

    println!("\n=== [TRANSFER_OUT_PRECOMPUTE] ===");
    println!("  Sender: {sender_addr}");
    println!("  Receiver: {receiver_addr}");

    // Build TxUtxo: sender → receiver (5.00) + change to sender (4.50) + fee (0.50)
    let tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: "tx1".into(),
                index: 0,
            },
        }],
        outputs: vec![
            TxOutput::new(receiver_addr.to_string(), "5.00".to_string(), None),
            TxOutput::new(sender_addr.to_string(), "4.50".to_string(), None),
            TxOutput::new(admin_addr.to_string(), "0.50".to_string(), None),
        ],
        fee: "0.50".to_string(),
        unlocks: vec![],
    };

    let plain = PlainPayload::TxUtxo(tx);

    // Pre-compute activity items with sender resolved (simulates encrypted handler)
    let mut addrs = pms_storage::helpers::extract_involved_addresses(&plain);
    let mut typed = pms_storage::helpers::extract_involved_with_category(&plain);
    // Sender might not be in output addresses if no change — add it
    if !addrs.contains(&sender_addr.to_string()) {
        addrs.push(sender_addr.to_string());
        typed.push((sender_addr.to_string(), pms_storage::helpers::ActivityCategory::Transfer));
    }
    println!("  Involved addresses: {:?}", addrs);

    let precomputed = pms_storage::helpers::precompute_all_items(
        &plain, &addrs, Some(sender_addr),
    );

    // Debug: show what was precomputed
    for (addr, items) in &precomputed {
        for item in items {
            println!(
                "  Precomputed for {}: type={}, dir={}, amount={:?}, counterparty={:?}",
                addr, item.activity_type, item.direction, item.amount, item.counterparty
            );
        }
    }

    // === Sender should get transfer_out ===
    let sender_items = precomputed.get(sender_addr).expect("sender should have items");
    assert_eq!(sender_items.len(), 1, "sender should have exactly 1 item");
    let si = &sender_items[0];
    assert_eq!(si.activity_type, "transfer_out");
    assert_eq!(si.direction, "out");
    // Amount = sum of non-sender outputs = 5.00 + 0.50 = 5.50
    assert_eq!(si.amount.as_deref(), Some("5.50"));
    assert_eq!(si.counterparty.as_deref(), Some(receiver_addr));
    println!(
        "  [OK] sender: type={}, dir={}, amount={:?}, counterparty={:?}",
        si.activity_type, si.direction, si.amount, si.counterparty
    );

    // === Receiver should get transfer_in ===
    let recv_items = precomputed.get(receiver_addr).expect("receiver should have items");
    assert_eq!(recv_items.len(), 1, "receiver should have exactly 1 item");
    let ri = &recv_items[0];
    assert_eq!(ri.activity_type, "transfer_in");
    assert_eq!(ri.direction, "in");
    assert_eq!(ri.amount.as_deref(), Some("5.00"));
    assert_eq!(ri.counterparty.as_deref(), Some(sender_addr));
    println!(
        "  [OK] receiver: type={}, dir={}, amount={:?}, counterparty={:?}",
        ri.activity_type, ri.direction, ri.amount, ri.counterparty
    );

    // === Admin (fee recipient) should get fee_received ===
    // Since admin_fee receives exactly the tx fee (0.50), it is classified as fee_received
    let admin_items = precomputed.get(admin_addr).expect("admin should have items");
    assert_eq!(admin_items.len(), 1);
    let ai = &admin_items[0];
    assert_eq!(ai.activity_type, "fee_received");
    assert_eq!(ai.amount.as_deref(), Some("0.50"));
    println!(
        "  [OK] admin: type={}, dir={}, amount={:?}",
        ai.activity_type, ai.direction, ai.amount
    );

    // === Without sender resolution → sender sees transfer_in (the bug we fixed) ===
    let precomputed_no_sender = pms_storage::helpers::precompute_all_items(
        &plain, &addrs, None,
    );
    let sender_no_resolve = precomputed_no_sender.get(sender_addr)
        .expect("sender should have items even without sender resolution");
    assert_eq!(sender_no_resolve[0].activity_type, "transfer_in",
        "without sender resolution, sender sees transfer_in (known limitation of plain path)");
    println!(
        "  [OK] without sender resolution: sender sees {} (expected behavior for plain path)",
        sender_no_resolve[0].activity_type
    );

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 5: Seize + seize_received appears in activity
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_seize_and_seize_received() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let victim = Wallet::from_seed(&[50u8; 32], None).unwrap();
    let victim_addr = victim.get_address(hrp);

    println!("\n=== [SEIZE] Setup ===");
    println!("  Victim: {victim_addr}");

    mint_to_wallet_and_get_inputs(&ctx, &victim, "50.00").await?;
    println!("  Minted 50.00 to victim");

    let seize_body = serde_json::json!({
        "address": victim_addr,
        "reason": "court order",
    });
    let (status, seize_json) =
        post_json_admin(&ctx.app, "/admin/compliance/seize", ADMIN_TOKEN, seize_body).await;
    print_api("SEIZE", status, &seize_json);
    assert!(status.is_success(), "seize failed: {status} body={seize_json}");

    let treasury_addr = seize_json["treasury_address"]
        .as_str()
        .expect("seize response should include treasury_address")
        .to_string();
    println!("  Treasury: {treasury_addr}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Victim should see "mint" + "seized"
    let path = format!("/v1/wallet/{}/activity", victim_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("SEIZED_VICTIM", &victim_addr, &json);

    assert!(status.is_success(), "victim activity failed: {status} body={json}");
    let items = json["items"].as_array().expect("items should be an array");

    let has_mint = items.iter().any(|i| i["activity_type"] == "mint");
    let has_seized = items.iter().any(|i| i["activity_type"] == "seized");
    assert!(has_mint, "victim should have 'mint' in activity, items={json}");
    assert!(has_seized, "victim should have 'seized' in activity, items={json}");

    let seized_item = items.iter().find(|i| i["activity_type"] == "seized").unwrap();
    assert_eq!(seized_item["direction"], "out");
    println!("  [OK] seized: direction={}, amount={}", seized_item["direction"], seized_item["amount"]);

    // Treasury should see "seize_received"
    let path = format!("/v1/wallet/{}/activity", treasury_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("SEIZE_RECEIVED_TREASURY", &treasury_addr, &json);

    assert!(status.is_success(), "treasury activity failed: {status} body={json}");
    let items = json["items"].as_array().expect("items should be an array");

    let has_seize_received = items.iter().any(|i| i["activity_type"] == "seize_received");
    assert!(has_seize_received, "treasury should have 'seize_received', items={json}");

    let sr = items.iter().find(|i| i["activity_type"] == "seize_received").unwrap();
    assert_eq!(sr["direction"], "in");
    assert_eq!(sr["counterparty"], victim_addr);
    println!("  [OK] seize_received: direction={}, counterparty={}", sr["direction"], sr["counterparty"]);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 6: Freeze appears in activity
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_freeze_appears() -> anyhow::Result<()> {
    set_admin_token_env();
    let ctx = make_test_ctx().await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let target = Wallet::from_seed(&[60u8; 32], None).unwrap();
    let target_addr = target.get_address(hrp);

    println!("\n=== [FREEZE] Setup ===");
    println!("  Target: {target_addr}");

    let freeze_body = serde_json::json!({
        "address": target_addr,
        "reason": "suspicious activity",
    });
    let (status, json) =
        post_json_admin(&ctx.app, "/admin/compliance/freeze", ADMIN_TOKEN, freeze_body).await;
    print_api("FREEZE", status, &json);
    assert!(status.is_success(), "freeze failed: {status} body={json}");

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let path = format!("/v1/wallet/{}/activity", target_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("FREEZE", &target_addr, &json);

    assert!(status.is_success(), "activity query failed: {status} body={json}");
    let items = json["items"].as_array().expect("items should be an array");

    let has_freeze = items.iter().any(|i| i["activity_type"] == "freeze");
    assert!(has_freeze, "expected 'freeze' in activity, items={json}");

    let freeze_item = items.iter().find(|i| i["activity_type"] == "freeze").unwrap();
    assert_eq!(freeze_item["direction"], "info");
    println!("  [OK] freeze: direction={}", freeze_item["direction"]);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 7: Unfreeze appears in activity (freeze then unfreeze)
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_unfreeze_appears() -> anyhow::Result<()> {
    set_admin_token_env();
    let ctx = make_test_ctx().await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let target = Wallet::from_seed(&[120u8; 32], None).unwrap();
    let target_addr = target.get_address(hrp);

    println!("\n=== [UNFREEZE] Setup ===");
    println!("  Target: {target_addr}");

    // Step 1: Freeze
    let freeze_body = serde_json::json!({
        "address": target_addr,
        "reason": "investigation pending",
    });
    let (status, freeze_json) =
        post_json_admin(&ctx.app, "/admin/compliance/freeze", ADMIN_TOKEN, freeze_body).await;
    print_api("FREEZE (setup)", status, &freeze_json);
    assert!(status.is_success(), "freeze failed: {status} body={freeze_json}");

    let freeze_block_id = freeze_json["block_id"]
        .as_str()
        .expect("freeze response should include block_id")
        .to_string();
    println!("  Freeze block_id: {freeze_block_id}");

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Step 2: Unfreeze
    let unfreeze_body = serde_json::json!({
        "address": target_addr,
        "reason": "investigation cleared",
        "freeze_block_id": freeze_block_id,
    });
    let (status, unfreeze_json) =
        post_json_admin(&ctx.app, "/admin/compliance/unfreeze", ADMIN_TOKEN, unfreeze_body).await;
    print_api("UNFREEZE", status, &unfreeze_json);
    assert!(status.is_success(), "unfreeze failed: {status} body={unfreeze_json}");

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Query activity - should see both freeze and unfreeze
    let path = format!("/v1/wallet/{}/activity", target_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("UNFREEZE", &target_addr, &json);

    assert!(status.is_success(), "activity query failed: {status} body={json}");
    let items = json["items"].as_array().expect("items should be an array");

    let has_freeze = items.iter().any(|i| i["activity_type"] == "freeze");
    let has_unfreeze = items.iter().any(|i| i["activity_type"] == "unfreeze");
    assert!(has_freeze, "expected 'freeze' in activity, items={json}");
    assert!(has_unfreeze, "expected 'unfreeze' in activity, items={json}");

    let unfreeze_item = items.iter().find(|i| i["activity_type"] == "unfreeze").unwrap();
    assert_eq!(unfreeze_item["direction"], "info");
    println!("  [OK] unfreeze: direction={}", unfreeze_item["direction"]);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 8: Fee received appears in activity
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_fee_received_appears() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let w_from = Wallet::from_seed(&[70u8; 32], None).unwrap();
    let w_to = Wallet::from_seed(&[71u8; 32], None).unwrap();
    let from_addr = w_from.get_address(hrp);
    let to_addr = w_to.get_address(hrp);

    println!("\n=== [FEE_RECEIVED] Setup ===");
    println!("  Sender: {from_addr}");
    println!("  Receiver: {to_addr}");
    println!("  Admin (fee collector): {admin_addr}");

    let (inputs, _) = mint_to_wallet_and_get_inputs(&ctx, &w_from, "5.00").await?;
    let u = inputs.first().expect("need at least one input");
    println!("  Minted 5.00 to sender, UTXO: {}#{}", u.id.txid, u.id.index);

    // Force manual UTXO persistence
    {
        let ua = pms_storage::rocks_store::utxo::UtxoApply {
            txid: u.id.txid.clone(),
            inputs: vec![],
            outputs: vec![(from_addr.clone(), u.amount.clone(), None)],
        };
        ctx.store.utxo_apply_tx_atomic(&ua).await?;
    }

    let taxable_amount = "4.00";
    let fee_policy =
        pms_token::fee::FeePolicy::new(&ctx.settings.fees.base_fee, &ctx.settings.fees.ratio);
    let fee_dec = fee_policy.compute_fee(taxable_amount).expect("fee computation");
    let fee = fee_dec.to_string();

    let input_dec: rust_decimal::Decimal = u.amount.parse().unwrap();
    let taxable_dec: rust_decimal::Decimal = taxable_amount.parse().unwrap();
    let change_dec = input_dec - taxable_dec - fee_dec.inner();
    let change = change_dec.normalize().to_string();
    println!("  Fee: {fee}, Change: {change}");

    let tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: u.id.txid.clone(),
                index: u.id.index,
            },
        }],
        outputs: vec![
            TxOutput::new(to_addr.clone(), taxable_amount.to_string(), None),
            TxOutput::new(admin_addr.clone(), fee.clone(), None),
            TxOutput::new(from_addr.clone(), change.clone(), None),
        ],
        fee: fee.clone(),
        unlocks: vec![],
    };
    let signed = sign_tx_inputs(&w_from, &tx, &ctx.settings.network.network_id);
    let body = serde_json::json!({
        "tx": serde_json::to_value(&signed).unwrap(),
        "recipients_xpk": [ w_to.x25519_pub_hex.clone() ]
    });
    let (status, json) = post_json(&ctx.app, "/wallet/tx/send", body).await;
    print_api("SEND (triggers Reward)", status, &json);
    assert!(status.is_success(), "send failed: {status} body={json}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Admin should see fee_received from the Reward block
    let path = format!("/v1/wallet/{}/activity", admin_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("FEE_RECEIVED", &admin_addr, &json);

    assert!(status.is_success(), "admin activity failed: {status} body={json}");
    let items = json["items"].as_array().expect("items should be an array");

    let has_fee = items.iter().any(|i| i["activity_type"] == "fee_received");
    assert!(has_fee, "admin should have 'fee_received' in activity, items={json}");

    let fee_item = items.iter().find(|i| i["activity_type"] == "fee_received").unwrap();
    assert_eq!(fee_item["direction"], "in");
    println!("  [OK] fee_received: direction={}, amount={}", fee_item["direction"], fee_item["amount"]);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 9: Reward appears in activity (forged Reward block with reward_outputs)
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_reward_appears() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();
    let meta = WireMeta::from(&ctx.settings);

    let wallet = Wallet::from_seed(&[130u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    println!("\n=== [REWARD] Setup ===");
    println!("  Reward recipient: {addr}");

    let tips = get_tips(&ctx).await;

    let reward_payload = PlainPayload::Reward {
        fee_outputs: vec![],
        reward_outputs: vec![TxOutput::new(addr.clone(), "5.00".to_string(), None)],
        burned: "0.50".to_string(),
        tx_block_id: "fake-tx-ref-for-test".to_string(),
    };

    let wb = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        3,
        Some(PayloadEnvelope::Plain(reward_payload)),
    );
    println!("  Forged Reward block: {}", wb.id);

    let res = ctx.srv.adapter_arc().persist_block(&wb).await?;
    println!("  Persist result: {res:?}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let path = format!("/v1/wallet/{}/activity", addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("REWARD", &addr, &json);

    assert!(status.is_success(), "activity query failed: {status} body={json}");
    let items = json["items"].as_array().expect("items should be an array");

    let reward_item = items.iter().find(|i| i["activity_type"] == "reward");
    assert!(reward_item.is_some(), "expected 'reward' in activity, items={json}");

    let r = reward_item.unwrap();
    assert_eq!(r["direction"], "in");
    assert_eq!(r["amount"], "5.00");
    println!("  [OK] reward: direction={}, amount={}", r["direction"], r["amount"]);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 10: Token create appears in activity
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_token_create_appears() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();
    let meta = WireMeta::from(&ctx.settings);

    // Use a specific wallet as the "creator" so we can query its activity
    let creator_wallet = Wallet::from_seed(&[135u8; 32], None).unwrap();
    let creator_addr = creator_wallet.get_address(hrp);

    println!("\n=== [TOKEN_CREATE] Setup ===");
    println!("  Creator address: {creator_addr}");

    let tips = get_tips(&ctx).await;

    let token_meta = TokenMetadata {
        asset_id: "testtkn".to_string(),
        symbol: "TST".to_string(),
        name: "Test Token".to_string(),
        decimals: 8,
        max_supply: None,
        creator: creator_addr.clone(),
        mint_authority: ctx.node_wallet.encoded_public_key(),
        demurrage_bps_per_day: None,
        collateral_address: None,
        collateral_asset_id: None,
        collateral_ratio_bps: None,
    };

    let wb = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        4,
        Some(PayloadEnvelope::Plain(PlainPayload::TokenCreate(
            token_meta,
        ))),
    );
    println!("  Forged TokenCreate block: {}", wb.id);

    let res = ctx.srv.adapter_arc().persist_block(&wb).await?;
    println!("  Persist result: {res:?}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let path = format!("/v1/wallet/{}/activity", creator_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("TOKEN_CREATE", &creator_addr, &json);

    assert!(status.is_success(), "activity query failed: {status} body={json}");
    let items = json["items"].as_array().expect("items should be an array");

    let tc_item = items.iter().find(|i| i["activity_type"] == "token_create");
    assert!(tc_item.is_some(), "expected 'token_create' in activity, items={json}");

    let tc = tc_item.unwrap();
    assert_eq!(tc["direction"], "info");
    assert_eq!(tc["asset_id"], "testtkn");
    println!("  [OK] token_create: direction={}, asset_id={}", tc["direction"], tc["asset_id"]);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 11: Reverse received appears in activity
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_reverse_received_appears() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();
    let meta = WireMeta::from(&ctx.settings);

    let sender = Wallet::from_seed(&[140u8; 32], None).unwrap();
    let receiver = Wallet::from_seed(&[141u8; 32], None).unwrap();
    let sender_addr = sender.get_address(hrp);
    let receiver_addr = receiver.get_address(hrp);

    println!("\n=== [REVERSE] Setup ===");
    println!("  Sender: {sender_addr}");
    println!("  Receiver: {receiver_addr}");

    // Mint 10.00 to sender
    let (inputs, _) = mint_to_wallet_and_get_inputs(&ctx, &sender, "10.00").await?;
    let u = &inputs[0];
    println!("  Minted 10.00, UTXO: {}#{}", u.id.txid, u.id.index);

    // Force UTXO persistence
    {
        let ua = pms_storage::rocks_store::utxo::UtxoApply {
            txid: u.id.txid.clone(),
            inputs: vec![],
            outputs: vec![(sender_addr.clone(), u.amount.clone(), None)],
        };
        ctx.store.utxo_apply_tx_atomic(&ua).await?;
    }

    // Forge a TxUtxo: sender → receiver + fee to admin
    // NOTE: sum(outputs) must == sum(inputs), fee goes to admin as output
    let tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: u.id.txid.clone(),
                index: u.id.index,
            },
        }],
        outputs: vec![
            TxOutput::new(receiver_addr.clone(), "9.50".to_string(), None),
            TxOutput::new(admin_addr.clone(), "0.50".to_string(), None),
        ],
        fee: "0.50".to_string(),
        unlocks: vec![],
    };

    let tips = get_tips(&ctx).await;
    let wb = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        2,
        Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(sign_tx_inputs(
            &sender,
            &tx,
            &meta.network_id,
        )))),
    );
    let tx_block_id = wb.id.clone();
    println!("  Forged TxUtxo block: {tx_block_id}");

    let res = ctx.srv.adapter_arc().persist_block(&wb).await?;
    println!("  Persist result: {res:?}");

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Now reverse the transaction
    let reverse_body = serde_json::json!({
        "block_id": tx_block_id,
        "reason": "fraudulent transaction",
    });
    let (status, reverse_json) =
        post_json_admin(&ctx.app, "/admin/compliance/reverse", ADMIN_TOKEN, reverse_body).await;
    print_api("REVERSE", status, &reverse_json);
    assert!(status.is_success(), "reverse failed: {status} body={reverse_json}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Sender should see reverse_received (funds returned)
    let path = format!("/v1/wallet/{}/activity", sender_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("REVERSE_RECEIVED_SENDER", &sender_addr, &json);

    assert!(status.is_success(), "sender activity failed: {status} body={json}");
    let items = json["items"].as_array().expect("items should be an array");

    let reverse_item = items.iter().find(|i| i["activity_type"] == "reverse_received");
    assert!(
        reverse_item.is_some(),
        "expected 'reverse_received' for sender, items={json}"
    );

    let rev = reverse_item.unwrap();
    assert_eq!(rev["direction"], "in");
    println!(
        "  [OK] reverse_received: direction={}, amount={}",
        rev["direction"], rev["amount"]
    );

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 12: NFT mint appears in activity
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_nft_mint_appears() -> anyhow::Result<()> {
    let ctx = make_test_ctx().await?;
    let _hrp = ctx.settings.address.hrp.as_str();
    let meta = WireMeta::from(&ctx.settings);

    // NFT validation: creator must == signer_pk_hex (hex pubkey, NOT bech32 address)
    let addr = ctx.node_wallet.encoded_public_key();

    println!("\n=== [NFT_MINT] Setup ===");
    println!("  Creator (hex pubkey): {addr}");

    let tips = get_tips(&ctx).await;

    let nft_mint = NftAction::Mint {
        token_id: "nft-e2e-001".to_string(),
        creator: addr.clone(),
        metadata: NftMetadata {
            name: Some("Test NFT".to_string()),
            description: Some("E2E test NFT".to_string()),
            uri: None,
            nft_type: Some("test".to_string()),
            extra: None,
        },
    };

    let wb = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        5,
        Some(PayloadEnvelope::Plain(PlainPayload::Nft(nft_mint))),
    );
    println!("  Forged NFT Mint block: {}", wb.id);

    let res = ctx.srv.adapter_arc().persist_block(&wb).await?;
    println!("  Persist result: {res:?}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let path = format!("/v1/wallet/{}/activity", addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("NFT_MINT", &addr, &json);

    assert!(status.is_success(), "activity query failed: {status} body={json}");
    let items = json["items"].as_array().expect("items should be an array");

    let nft_item = items.iter().find(|i| i["activity_type"] == "nft_mint");
    assert!(nft_item.is_some(), "expected 'nft_mint' in activity, items={json}");

    let nm = nft_item.unwrap();
    assert_eq!(nm["direction"], "in");
    println!("  [OK] nft_mint: direction={}", nm["direction"]);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 13: NFT transfer appears (nft_transfer_out + nft_transfer_in)
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_nft_transfer_in_out() -> anyhow::Result<()> {
    let ctx = make_test_ctx().await?;
    let _hrp = ctx.settings.address.hrp.as_str();
    let meta = WireMeta::from(&ctx.settings);

    // NFT validation: creator/from must == signer_pk_hex
    let sender_addr = ctx.node_wallet.encoded_public_key();
    let receiver = Wallet::from_seed(&[161u8; 32], None).unwrap();
    let receiver_addr = receiver.encoded_public_key();

    println!("\n=== [NFT_TRANSFER] Setup ===");
    println!("  From (hex pubkey): {sender_addr}");
    println!("  To (hex pubkey): {receiver_addr}");

    // First, mint the NFT to sender (node_wallet hex pubkey)
    let tips = get_tips(&ctx).await;
    let nft_mint = NftAction::Mint {
        token_id: "nft-e2e-002".to_string(),
        creator: sender_addr.clone(),
        metadata: NftMetadata {
            name: Some("Transfer Test NFT".to_string()),
            description: None,
            uri: None,
            nft_type: Some("test".to_string()),
            extra: None,
        },
    };
    let wb_mint = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        5,
        Some(PayloadEnvelope::Plain(PlainPayload::Nft(nft_mint))),
    );
    println!("  Forged NFT Mint block: {}", wb_mint.id);
    let res = ctx.srv.adapter_arc().persist_block(&wb_mint).await?;
    println!("  Mint persist: {res:?}");

    // Then transfer it
    let tips = get_tips(&ctx).await;
    let nft_transfer = NftAction::Transfer {
        token_id: "nft-e2e-002".to_string(),
        from: sender_addr.clone(),
        to: receiver_addr.clone(),
        new_owner_x25519_pubkey: None,
        encrypted_metadata: None,
    };
    let wb_transfer = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        6,
        Some(PayloadEnvelope::Plain(PlainPayload::Nft(nft_transfer))),
    );
    println!("  Forged NFT Transfer block: {}", wb_transfer.id);
    let res = ctx.srv.adapter_arc().persist_block(&wb_transfer).await?;
    println!("  Transfer persist: {res:?}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Sender should see nft_mint + nft_transfer_out
    let path = format!("/v1/wallet/{}/activity", sender_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("NFT_TRANSFER_OUT (sender)", &sender_addr, &json);

    assert!(status.is_success());
    let items = json["items"].as_array().expect("items");
    let nft_out = items.iter().find(|i| i["activity_type"] == "nft_transfer_out");
    assert!(nft_out.is_some(), "expected 'nft_transfer_out' for sender, items={json}");

    let no = nft_out.unwrap();
    assert_eq!(no["direction"], "out");
    assert_eq!(no["counterparty"], receiver_addr);
    println!("  [OK] nft_transfer_out: direction={}, counterparty={}", no["direction"], no["counterparty"]);

    // Receiver should see nft_transfer_in
    let path = format!("/v1/wallet/{}/activity", receiver_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("NFT_TRANSFER_IN (receiver)", &receiver_addr, &json);

    assert!(status.is_success());
    let items = json["items"].as_array().expect("items");
    let nft_in = items.iter().find(|i| i["activity_type"] == "nft_transfer_in");
    assert!(nft_in.is_some(), "expected 'nft_transfer_in' for receiver, items={json}");

    let ni = nft_in.unwrap();
    assert_eq!(ni["direction"], "in");
    assert_eq!(ni["counterparty"], sender_addr);
    println!("  [OK] nft_transfer_in: direction={}, counterparty={}", ni["direction"], ni["counterparty"]);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 14: NFT burn appears in activity
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_nft_burn_appears() -> anyhow::Result<()> {
    let ctx = make_test_ctx().await?;
    let _hrp = ctx.settings.address.hrp.as_str();
    let meta = WireMeta::from(&ctx.settings);

    // NFT validation: creator/burner must == signer_pk_hex
    let addr = ctx.node_wallet.encoded_public_key();

    println!("\n=== [NFT_BURN] Setup ===");
    println!("  Burner (hex pubkey): {addr}");

    // Mint NFT
    let tips = get_tips(&ctx).await;
    let nft_mint = NftAction::Mint {
        token_id: "nft-e2e-003".to_string(),
        creator: addr.clone(),
        metadata: NftMetadata {
            name: Some("Burn Test NFT".to_string()),
            description: None,
            uri: None,
            nft_type: Some("test".to_string()),
            extra: None,
        },
    };
    let wb_mint = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        7,
        Some(PayloadEnvelope::Plain(PlainPayload::Nft(nft_mint))),
    );
    println!("  Forged NFT Mint block: {}", wb_mint.id);
    let res = ctx.srv.adapter_arc().persist_block(&wb_mint).await?;
    println!("  Mint persist: {res:?}");

    // Burn NFT
    let tips = get_tips(&ctx).await;
    let nft_burn = NftAction::Burn {
        token_id: "nft-e2e-003".to_string(),
        burner: addr.clone(),
    };
    let wb_burn = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        8,
        Some(PayloadEnvelope::Plain(PlainPayload::Nft(nft_burn))),
    );
    println!("  Forged NFT Burn block: {}", wb_burn.id);
    let res = ctx.srv.adapter_arc().persist_block(&wb_burn).await?;
    println!("  Burn persist: {res:?}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let path = format!("/v1/wallet/{}/activity", addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("NFT_BURN", &addr, &json);

    assert!(status.is_success(), "activity query failed: {status} body={json}");
    let items = json["items"].as_array().expect("items");

    let has_mint = items.iter().any(|i| i["activity_type"] == "nft_mint");
    let has_burn = items.iter().any(|i| i["activity_type"] == "nft_burn");
    assert!(has_mint, "expected 'nft_mint' in activity, items={json}");
    assert!(has_burn, "expected 'nft_burn' in activity, items={json}");

    let burn = items.iter().find(|i| i["activity_type"] == "nft_burn").unwrap();
    assert_eq!(burn["direction"], "out");
    println!("  [OK] nft_burn: direction={}", burn["direction"]);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 15: NFT use appears in activity
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_nft_use_appears() -> anyhow::Result<()> {
    let ctx = make_test_ctx().await?;
    let _hrp = ctx.settings.address.hrp.as_str();
    let meta = WireMeta::from(&ctx.settings);

    // NFT validation: creator/user must == signer_pk_hex
    let addr = ctx.node_wallet.encoded_public_key();

    println!("\n=== [NFT_USE] Setup ===");
    println!("  User (hex pubkey): {addr}");

    // Mint NFT
    let tips = get_tips(&ctx).await;
    let nft_mint = NftAction::Mint {
        token_id: "nft-e2e-004".to_string(),
        creator: addr.clone(),
        metadata: NftMetadata {
            name: Some("Use Test NFT".to_string()),
            description: None,
            uri: None,
            nft_type: Some("ticket".to_string()),
            extra: None,
        },
    };
    let wb_mint = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        9,
        Some(PayloadEnvelope::Plain(PlainPayload::Nft(nft_mint))),
    );
    println!("  Forged NFT Mint block: {}", wb_mint.id);
    let res = ctx.srv.adapter_arc().persist_block(&wb_mint).await?;
    println!("  Mint persist: {res:?}");

    // Use NFT
    let tips = get_tips(&ctx).await;
    let nft_use = NftAction::Use {
        token_id: "nft-e2e-004".to_string(),
        user: addr.clone(),
        action_type: "redeem".to_string(),
        action_data: Some("e2e test usage".to_string()),
    };
    let wb_use = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        10,
        Some(PayloadEnvelope::Plain(PlainPayload::Nft(nft_use))),
    );
    println!("  Forged NFT Use block: {}", wb_use.id);
    let res = ctx.srv.adapter_arc().persist_block(&wb_use).await?;
    println!("  Use persist: {res:?}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let path = format!("/v1/wallet/{}/activity", addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("NFT_USE", &addr, &json);

    assert!(status.is_success(), "activity query failed: {status} body={json}");
    let items = json["items"].as_array().expect("items");

    let use_item = items.iter().find(|i| i["activity_type"] == "nft_use");
    assert!(use_item.is_some(), "expected 'nft_use' in activity, items={json}");

    let nu = use_item.unwrap();
    assert_eq!(nu["direction"], "info");
    println!("  [OK] nft_use: direction={}", nu["direction"]);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 16: Bridge lock_in appears in activity
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_bridge_lock_in_appears() -> anyhow::Result<()> {
    // setup_admin_ctx + _with_admin makes node_wallet the admin/coordinator
    // (seed 7) so the mint passes (PMS_TEST_ADMIN_PUBKEY) and the coordinator-only
    // BridgeLock is authorized.
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();
    let meta = WireMeta::from(&ctx.settings);

    let wallet = Wallet::from_seed(&[190u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    println!("\n=== [BRIDGE_LOCK_IN] Setup ===");
    println!("  Destination: {addr}");

    // BridgeLock requires actual UTXO inputs. Mint first to node_wallet.
    let node_addr = ctx.node_wallet.get_address(hrp);
    let (inputs, _) = mint_to_wallet_and_get_inputs(&ctx, &Wallet::from_seed(&[7u8; 32], None).unwrap(), "100.00").await?;
    let u = &inputs[0];
    println!("  Minted 100.00 for bridge lock, UTXO: {}#{}", u.id.txid, u.id.index);

    // Force UTXO persistence
    {
        let ua = pms_storage::rocks_store::utxo::UtxoApply {
            txid: u.id.txid.clone(),
            inputs: vec![],
            outputs: vec![(node_addr.clone(), u.amount.clone(), None)],
        };
        ctx.store.utxo_apply_tx_atomic(&ua).await?;
    }

    let tips = get_tips(&ctx).await;

    let bridge_lock = PlainPayload::BridgeLock {
        inputs: vec![TxInput {
            out: OutputId {
                txid: u.id.txid.clone(),
                index: u.id.index,
            },
        }],
        amount: "100.00".to_string(),
        asset_id: None,
        dest_ledger_id: "side-ledger".to_string(),
        dest_address: addr.clone(),
    };

    let wb = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        11,
        Some(PayloadEnvelope::Plain(bridge_lock)),
    );
    println!("  Forged BridgeLock block: {}", wb.id);

    let res = ctx.srv.adapter_arc().persist_block(&wb).await?;
    println!("  Persist result: {res:?}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let path = format!("/v1/wallet/{}/activity", addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("BRIDGE_LOCK_IN", &addr, &json);

    assert!(status.is_success(), "activity query failed: {status} body={json}");
    let items = json["items"].as_array().expect("items");

    let lock_item = items.iter().find(|i| i["activity_type"] == "bridge_lock_in");
    assert!(lock_item.is_some(), "expected 'bridge_lock_in' in activity, items={json}");

    let bl = lock_item.unwrap();
    assert_eq!(bl["direction"], "in");
    assert_eq!(bl["amount"], "100.00");
    println!(
        "  [OK] bridge_lock_in: direction={}, amount={}, asset_id={}",
        bl["direction"], bl["amount"], bl["asset_id"]
    );

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 17: Bridge mint appears in activity
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_bridge_mint_appears() -> anyhow::Result<()> {
    let ctx = make_test_ctx().await?;
    let hrp = ctx.settings.address.hrp.as_str();
    let meta = WireMeta::from(&ctx.settings);

    let wallet = Wallet::from_seed(&[195u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    println!("\n=== [BRIDGE_MINT] Setup ===");
    println!("  Recipient: {addr}");

    let tips = get_tips(&ctx).await;

    let bridge_mint = PlainPayload::BridgeMint {
        outputs: vec![TxOutput::new(addr.clone(), "100.00".to_string(), None)],
        lock_block_id: "fake-lock-blk-for-test".to_string(),
        source_ledger_id: "main".to_string(),
    };

    let wb = forge_signed_wire_block_for_test(
        tips,
        &meta,
        &ctx.node_wallet,
        12,
        Some(PayloadEnvelope::Plain(bridge_mint)),
    );
    println!("  Forged BridgeMint block: {}", wb.id);

    let res = ctx.srv.adapter_arc().persist_block(&wb).await?;
    println!("  Persist result: {res:?}");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let path = format!("/v1/wallet/{}/activity", addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("BRIDGE_MINT", &addr, &json);

    assert!(status.is_success(), "activity query failed: {status} body={json}");
    let items = json["items"].as_array().expect("items");

    let mint_item = items.iter().find(|i| i["activity_type"] == "bridge_mint");
    assert!(mint_item.is_some(), "expected 'bridge_mint' in activity, items={json}");

    let bm = mint_item.unwrap();
    assert_eq!(bm["direction"], "in");
    assert_eq!(bm["amount"], "100.00");
    println!("  [OK] bridge_mint: direction={}, amount={}", bm["direction"], bm["amount"]);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 18: Type filter works
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_type_filter_works() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let wallet = Wallet::from_seed(&[80u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    println!("\n=== [TYPE_FILTER] Setup ===");
    println!("  Wallet: {addr}");

    // Mint (creates "mint" activity)
    mint_to_wallet_and_get_inputs(&ctx, &wallet, "10.00").await?;
    println!("  Minted 10.00");

    // Also freeze this address (creates "freeze" activity)
    let freeze_body = serde_json::json!({
        "address": addr,
        "reason": "test filter",
    });
    let (status, json) =
        post_json_admin(&ctx.app, "/admin/compliance/freeze", ADMIN_TOKEN, freeze_body).await;
    print_api("FREEZE (setup)", status, &json);
    assert!(status.is_success());

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // No filter: should have both mint + freeze
    let path = format!("/v1/wallet/{}/activity", addr);
    let (_, json) = get_json(&ctx.app, &path).await;
    print_activity("NO_FILTER", &addr, &json);

    let items = json["items"].as_array().unwrap();
    assert!(items.len() >= 2, "expected at least 2 items without filter, got {}", items.len());

    // Filter by mint only
    let path = format!("/v1/wallet/{}/activity?type=mint", addr);
    let (_, json) = get_json(&ctx.app, &path).await;
    print_activity("FILTER=mint", &addr, &json);

    let items = json["items"].as_array().unwrap();
    assert!(
        items.iter().all(|i| i["activity_type"] == "mint"),
        "expected only 'mint' with type=mint filter, items={json}"
    );
    assert!(!items.is_empty(), "expected at least 1 mint item");
    println!("  [OK] type=mint filter: {} items, all mint", items.len());

    // Filter by freeze only
    let path = format!("/v1/wallet/{}/activity?type=freeze", addr);
    let (_, json) = get_json(&ctx.app, &path).await;
    print_activity("FILTER=freeze", &addr, &json);

    let items = json["items"].as_array().unwrap();
    assert!(
        items.iter().all(|i| i["activity_type"] == "freeze"),
        "expected only 'freeze' with type=freeze filter, items={json}"
    );
    assert!(!items.is_empty(), "expected at least 1 freeze item");
    println!("  [OK] type=freeze filter: {} items, all freeze", items.len());

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 19: Empty activity for unknown address
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn activity_empty_for_unknown_address() -> anyhow::Result<()> {
    let ctx = make_test_ctx().await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let wallet = Wallet::from_seed(&[99u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    println!("\n=== [EMPTY] Setup ===");
    println!("  Unknown address: {addr}");

    let path = format!("/v1/wallet/{}/activity", addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    print_activity("EMPTY", &addr, &json);

    assert!(status.is_success(), "activity query failed: {status} body={json}");

    let items = json["items"].as_array().expect("items should be an array");
    assert!(items.is_empty(), "expected empty items for unknown address, got {}", items.len());
    assert_eq!(json["count"], 0);
    assert_eq!(json["has_more"], false);

    println!("  [OK] empty: count=0, has_more=false");

    Ok(())
}
