use pms_testkit::{
    get_json, make_test_ctx, make_test_ctx_with_admin, mint_to_wallet_and_get_inputs,
    post_json,
};
use pms_wallet::{SignerBackend, Wallet};

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

fn setup_admin_ctx() -> (std::sync::Arc<Wallet>, String, String) {
    clear_admin_env_conflicts();
    let admin = std::sync::Arc::new(Wallet::from_seed(&[7u8; 32], None).unwrap());
    let hrp = "8e";
    let admin_addr = admin.get_address(hrp);
    let admin_pubkey = admin.encoded_public_key();
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &admin_pubkey);
    }
    (admin, admin_addr, admin_pubkey)
}

// ── Test 1: Mint appears in activity ──────────────────────────────────────

#[tokio::test]
async fn activity_mint_appears() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let wallet = Wallet::from_seed(&[42u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    mint_to_wallet_and_get_inputs(&ctx, &wallet, "10.00").await?;

    // Wait for background persist to write addr_activity entries
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let path = format!("/v1/wallet/{}/activity", addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    assert!(status.is_success(), "activity query failed: {status} body={json}");

    let items = json["items"].as_array().expect("items should be an array");
    assert!(!items.is_empty(), "expected at least 1 activity item, got 0");

    let mint_item = items.iter().find(|i| i["activity_type"] == "mint");
    assert!(mint_item.is_some(), "expected a 'mint' activity item, items={json}");

    let mint = mint_item.unwrap();
    assert_eq!(mint["direction"], "in");
    assert_eq!(mint["amount"], "10.00");

    Ok(())
}

// ── Test 2: Encrypted transfer via /wallet/tx/send appears ────────────────

#[tokio::test]
async fn activity_transfer_via_send_appears() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let w_from = Wallet::from_seed(&[10u8; 32], None).unwrap();
    let w_to = Wallet::from_seed(&[11u8; 32], None).unwrap();
    let from_addr = w_from.get_address(hrp);
    let to_addr = w_to.get_address(hrp);

    // Mint to sender
    let (inputs, _) = mint_to_wallet_and_get_inputs(&ctx, &w_from, "5.00").await?;
    let u = inputs.first().expect("need at least one input from mint");

    // Force manual UTXO persistence for the mint
    {
        let ua = pms_storage::rocks_store::utxo::UtxoApply {
            txid: u.id.txid.clone(),
            inputs: vec![],
            outputs: vec![(from_addr.clone(), u.amount.clone(), None)],
        };
        ctx.store.utxo_apply_tx_atomic(&ua).await?;
    }

    // Compute fee
    let taxable_amount = "4.00";
    let fee_policy =
        pms_token::fee::FeePolicy::new(&ctx.settings.fees.base_fee, &ctx.settings.fees.ratio);
    let fee_dec = fee_policy.compute_fee(taxable_amount).expect("fee computation");
    let fee = fee_dec.to_string();

    let input_dec: rust_decimal::Decimal = u.amount.parse().unwrap();
    let taxable_dec: rust_decimal::Decimal = taxable_amount.parse().unwrap();
    let change_dec = input_dec - taxable_dec - fee_dec.inner();
    let change = change_dec.normalize().to_string();

    let body = serde_json::json!({
        "tx": {
            "inputs": [{ "out": { "txid": u.id.txid, "index": u.id.index } }],
            "outputs": [
                { "address": to_addr, "amount": taxable_amount },
                { "address": admin_addr, "amount": &fee },
                { "address": from_addr, "amount": &change }
            ],
            "fee": fee,
            "unlocks": []
        },
        "recipients_xpk": [ w_to.x25519_pub_hex.clone() ]
    });
    let (status, json) = post_json(&ctx.app, "/wallet/tx/send", body).await;
    assert!(status.is_success(), "send failed: {status} body={json}");

    // Small delay for background persist
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Receiver should see transfer_in via encrypted payload decryption
    let path = format!(
        "/v1/wallet/{}/activity?x25519_sk_hex={}",
        to_addr,
        w_to.x25519_sk_hex().unwrap()
    );
    let (status, json) = get_json(&ctx.app, &path).await;
    assert!(status.is_success(), "receiver activity failed: {status} body={json}");

    let items = json["items"].as_array().expect("items should be an array");
    let transfer_in = items.iter().find(|i| i["activity_type"] == "transfer_in");
    assert!(
        transfer_in.is_some(),
        "expected 'transfer_in' for receiver, items={json}"
    );

    let ti = transfer_in.unwrap();
    assert_eq!(ti["direction"], "in");

    Ok(())
}

// ── Test 3: Seize appears in activity ─────────────────────────────────────

#[tokio::test]
async fn activity_seize_appears() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let victim = Wallet::from_seed(&[50u8; 32], None).unwrap();
    let victim_addr = victim.get_address(hrp);

    // Mint to victim
    mint_to_wallet_and_get_inputs(&ctx, &victim, "50.00").await?;

    // Seize via admin API (admin_token is None → auth bypassed)
    let seize_body = serde_json::json!({
        "address": victim_addr,
        "reason": "court order",
    });
    let (status, seize_json) = post_json(&ctx.app, "/admin/compliance/seize", seize_body).await;
    assert!(status.is_success(), "seize failed: {status} body={seize_json}");

    // The treasury address receiving seized funds may differ from admin_addr
    // (config.dev.toml sets fees.treasury_addresses)
    let treasury_addr = seize_json["treasury_address"]
        .as_str()
        .expect("seize response should include treasury_address")
        .to_string();

    // Wait for background persist to write addr_activity entries
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Victim should see "mint" + "seized" in activity
    let path = format!("/v1/wallet/{}/activity", victim_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    assert!(status.is_success(), "victim activity failed: {status} body={json}");

    let items = json["items"].as_array().expect("items should be an array");
    let has_mint = items.iter().any(|i| i["activity_type"] == "mint");
    let has_seized = items.iter().any(|i| i["activity_type"] == "seized");
    assert!(has_mint, "victim should have 'mint' in activity, items={json}");
    assert!(has_seized, "victim should have 'seized' in activity, items={json}");

    // Check seized item details
    let seized_item = items.iter().find(|i| i["activity_type"] == "seized").unwrap();
    assert_eq!(seized_item["direction"], "out");

    // Treasury address should see "seize_received"
    let path = format!("/v1/wallet/{}/activity", treasury_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    assert!(status.is_success(), "treasury activity failed: {status} body={json}");

    let items = json["items"].as_array().expect("items should be an array");
    let has_seize_received = items.iter().any(|i| i["activity_type"] == "seize_received");
    assert!(
        has_seize_received,
        "treasury should have 'seize_received' in activity, items={json}"
    );

    let sr = items
        .iter()
        .find(|i| i["activity_type"] == "seize_received")
        .unwrap();
    assert_eq!(sr["direction"], "in");
    assert_eq!(sr["counterparty"], victim_addr);

    Ok(())
}

// ── Test 4: Freeze appears in activity ────────────────────────────────────

#[tokio::test]
async fn activity_freeze_appears() -> anyhow::Result<()> {
    let ctx = make_test_ctx().await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let target = Wallet::from_seed(&[60u8; 32], None).unwrap();
    let target_addr = target.get_address(hrp);

    let freeze_body = serde_json::json!({
        "address": target_addr,
        "reason": "suspicious activity",
    });
    let (status, json) = post_json(&ctx.app, "/admin/compliance/freeze", freeze_body).await;
    assert!(status.is_success(), "freeze failed: {status} body={json}");

    // Small delay for background persist
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let path = format!("/v1/wallet/{}/activity", target_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    assert!(status.is_success(), "activity query failed: {status} body={json}");

    let items = json["items"].as_array().expect("items should be an array");
    let has_freeze = items.iter().any(|i| i["activity_type"] == "freeze");
    assert!(has_freeze, "expected 'freeze' in activity, items={json}");

    let freeze_item = items.iter().find(|i| i["activity_type"] == "freeze").unwrap();
    assert_eq!(freeze_item["direction"], "info");

    Ok(())
}

// ── Test 5: Fee received appears in activity ──────────────────────────────

#[tokio::test]
async fn activity_fee_received_appears() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let w_from = Wallet::from_seed(&[70u8; 32], None).unwrap();
    let w_to = Wallet::from_seed(&[71u8; 32], None).unwrap();
    let from_addr = w_from.get_address(hrp);
    let to_addr = w_to.get_address(hrp);

    // Mint to sender
    let (inputs, _) = mint_to_wallet_and_get_inputs(&ctx, &w_from, "5.00").await?;
    let u = inputs.first().expect("need at least one input");

    // Force manual UTXO persistence
    {
        let ua = pms_storage::rocks_store::utxo::UtxoApply {
            txid: u.id.txid.clone(),
            inputs: vec![],
            outputs: vec![(from_addr.clone(), u.amount.clone(), None)],
        };
        ctx.store.utxo_apply_tx_atomic(&ua).await?;
    }

    // Send transaction (triggers reward block creation)
    let taxable_amount = "4.00";
    let fee_policy =
        pms_token::fee::FeePolicy::new(&ctx.settings.fees.base_fee, &ctx.settings.fees.ratio);
    let fee_dec = fee_policy.compute_fee(taxable_amount).expect("fee computation");
    let fee = fee_dec.to_string();

    let input_dec: rust_decimal::Decimal = u.amount.parse().unwrap();
    let taxable_dec: rust_decimal::Decimal = taxable_amount.parse().unwrap();
    let change_dec = input_dec - taxable_dec - fee_dec.inner();
    let change = change_dec.normalize().to_string();

    let body = serde_json::json!({
        "tx": {
            "inputs": [{ "out": { "txid": u.id.txid, "index": u.id.index } }],
            "outputs": [
                { "address": to_addr, "amount": taxable_amount },
                { "address": admin_addr, "amount": &fee },
                { "address": from_addr, "amount": &change }
            ],
            "fee": fee,
            "unlocks": []
        },
        "recipients_xpk": [ w_to.x25519_pub_hex.clone() ]
    });
    let (status, json) = post_json(&ctx.app, "/wallet/tx/send", body).await;
    assert!(status.is_success(), "send failed: {status} body={json}");

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Admin should see fee_received from the Reward block
    let path = format!("/v1/wallet/{}/activity", admin_addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    assert!(status.is_success(), "admin activity failed: {status} body={json}");

    let items = json["items"].as_array().expect("items should be an array");
    let has_fee = items.iter().any(|i| i["activity_type"] == "fee_received");
    assert!(
        has_fee,
        "admin should have 'fee_received' in activity, items={json}"
    );

    let fee_item = items.iter().find(|i| i["activity_type"] == "fee_received").unwrap();
    assert_eq!(fee_item["direction"], "in");

    Ok(())
}

// ── Test 6: Type filter works ─────────────────────────────────────────────

#[tokio::test]
async fn activity_type_filter_works() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let wallet = Wallet::from_seed(&[80u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    // Mint (creates "mint" activity)
    mint_to_wallet_and_get_inputs(&ctx, &wallet, "10.00").await?;

    // Also freeze this address (creates "freeze" activity)
    let freeze_body = serde_json::json!({
        "address": addr,
        "reason": "test filter",
    });
    let (status, _) = post_json(&ctx.app, "/admin/compliance/freeze", freeze_body).await;
    assert!(status.is_success());

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // No filter: should have both mint + freeze
    let path = format!("/v1/wallet/{}/activity", addr);
    let (_, json) = get_json(&ctx.app, &path).await;
    let items = json["items"].as_array().unwrap();
    assert!(items.len() >= 2, "expected at least 2 items without filter, got {}", items.len());

    // Filter by mint only
    let path = format!("/v1/wallet/{}/activity?type=mint", addr);
    let (_, json) = get_json(&ctx.app, &path).await;
    let items = json["items"].as_array().unwrap();
    assert!(
        items.iter().all(|i| i["activity_type"] == "mint"),
        "expected only 'mint' with type=mint filter, items={json}"
    );
    assert!(!items.is_empty(), "expected at least 1 mint item");

    // Filter by freeze only
    let path = format!("/v1/wallet/{}/activity?type=freeze", addr);
    let (_, json) = get_json(&ctx.app, &path).await;
    let items = json["items"].as_array().unwrap();
    assert!(
        items.iter().all(|i| i["activity_type"] == "freeze"),
        "expected only 'freeze' with type=freeze filter, items={json}"
    );
    assert!(!items.is_empty(), "expected at least 1 freeze item");

    Ok(())
}

// ── Test 7: Empty activity for unknown address ────────────────────────────

#[tokio::test]
async fn activity_empty_for_unknown_address() -> anyhow::Result<()> {
    let ctx = make_test_ctx().await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let wallet = Wallet::from_seed(&[99u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    let path = format!("/v1/wallet/{}/activity", addr);
    let (status, json) = get_json(&ctx.app, &path).await;
    assert!(status.is_success(), "activity query failed: {status} body={json}");

    let items = json["items"].as_array().expect("items should be an array");
    assert!(items.is_empty(), "expected empty items for unknown address, got {}", items.len());
    assert_eq!(json["count"], 0);
    assert_eq!(json["has_more"], false);

    Ok(())
}
