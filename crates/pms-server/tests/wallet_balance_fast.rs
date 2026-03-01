use pms_testkit::{make_test_ctx, make_test_ctx_with_admin, mint_to_wallet_and_get_inputs, post_json};
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
    unsafe { std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &admin_pubkey); }
    (admin, admin_addr, admin_pubkey)
}

#[tokio::test]
async fn wallet_balance_returns_correct_balance_from_ram() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let wallet = Wallet::from_seed(&[42u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    // Mint 10 PMS to the wallet
    mint_to_wallet_and_get_inputs(&ctx, &wallet, "10.00").await?;

    // Query balance via POST /wallet/balance
    let body = serde_json::json!({
        "bech32_addr": addr,
        "x25519_sk_hex": wallet.x25519_sk_hex().unwrap(),
        "ecdsa_pk_hex": wallet.public_key_hex,
    });
    let (status, json) = post_json(&ctx.app, "/wallet/balance", body).await;

    assert!(status.is_success(), "balance query failed: {status} body={json}");

    // Balance is Decimal::to_string() — may or may not have trailing zeros
    let bal: rust_decimal::Decimal = json["balance"].as_str().unwrap().parse().unwrap();
    assert_eq!(bal, rust_decimal::Decimal::new(1000, 2), "balance mismatch: {json}");

    let utxos = json["utxos"].as_array().expect("utxos should be an array");
    assert_eq!(utxos.len(), 1, "expected 1 UTXO, got {}", utxos.len());

    Ok(())
}

#[tokio::test]
async fn wallet_balance_returns_zero_for_unknown_address() -> anyhow::Result<()> {
    let ctx = make_test_ctx().await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let wallet = Wallet::from_seed(&[99u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    let body = serde_json::json!({
        "bech32_addr": addr,
        "x25519_sk_hex": wallet.x25519_sk_hex().unwrap(),
        "ecdsa_pk_hex": wallet.public_key_hex,
    });
    let (status, json) = post_json(&ctx.app, "/wallet/balance", body).await;

    assert!(status.is_success(), "balance query failed: {status} body={json}");
    assert_eq!(json["balance"], "0", "expected zero balance: {json}");

    let utxos = json["utxos"].as_array().expect("utxos should be an array");
    assert!(utxos.is_empty(), "expected empty utxos, got {}", utxos.len());

    Ok(())
}

#[tokio::test]
async fn wallet_balance_matches_v1_balance() -> anyhow::Result<()> {
    let (_admin, admin_addr, admin_pubkey) = setup_admin_ctx();
    let ctx = make_test_ctx_with_admin(vec![admin_addr], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.as_str();

    let wallet = Wallet::from_seed(&[55u8; 32], None).unwrap();
    let addr = wallet.get_address(hrp);

    // Mint 25 PMS
    mint_to_wallet_and_get_inputs(&ctx, &wallet, "25.00").await?;

    // Query via POST /wallet/balance (the refactored endpoint)
    let body_wallet = serde_json::json!({
        "bech32_addr": addr,
        "x25519_sk_hex": wallet.x25519_sk_hex().unwrap(),
        "ecdsa_pk_hex": wallet.public_key_hex,
    });
    let (status1, json1) = post_json(&ctx.app, "/wallet/balance", body_wallet).await;
    assert!(status1.is_success(), "wallet/balance failed: {status1}");

    // Query via POST /v1/balance (already using RAM)
    let body_v1 = serde_json::json!({ "address": addr });
    let (status2, json2) = post_json(&ctx.app, "/v1/balance", body_v1).await;
    assert!(status2.is_success(), "/v1/balance failed: {status2}");

    // Both should return the same balance
    assert_eq!(
        json1["balance"], json2["balance"],
        "wallet/balance={} vs v1/balance={}",
        json1["balance"], json2["balance"]
    );

    Ok(())
}
