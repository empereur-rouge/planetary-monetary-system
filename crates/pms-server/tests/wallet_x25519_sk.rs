use pms_testkit::{make_test_ctx, post_json};

/// POST /v1/wallet/create must return x25519_sk_hex that is consistent
/// with x25519_pub_hex (i.e. derived from the same ECDSA private key).
#[tokio::test]
async fn wallet_create_returns_x25519_sk() {
    let ctx = make_test_ctx().await.unwrap();

    let (status, body) = post_json(&ctx.app, "/v1/wallet/create", serde_json::json!({})).await;
    println!("=== POST /v1/wallet/create ===");
    println!("Status: {status}");
    println!("Body: {}", serde_json::to_string_pretty(&body).unwrap());

    assert_eq!(status.as_u16(), 200);

    // x25519_sk_hex must be present and non-empty
    let sk = body["x25519_sk_hex"].as_str().expect("x25519_sk_hex missing");
    let pk = body["x25519_pub_hex"].as_str().expect("x25519_pub_hex missing");
    println!("x25519_sk_hex: {sk}");
    println!("x25519_pub_hex: {pk}");

    assert_eq!(sk.len(), 64, "x25519_sk_hex should be 64 hex chars (32 bytes)");
    assert_eq!(pk.len(), 64, "x25519_pub_hex should be 64 hex chars (32 bytes)");
    assert!(sk.chars().all(|c| c.is_ascii_hexdigit()), "sk must be hex");

    // Verify sk derives to pk (using x25519-dalek)
    let sk_bytes: [u8; 32] = hex::decode(sk).unwrap().try_into().unwrap();
    let derived_pk = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(sk_bytes));
    let derived_pk_hex = hex::encode(derived_pk.as_bytes());
    println!("Derived pk from sk: {derived_pk_hex}");
    assert_eq!(derived_pk_hex, pk, "x25519_sk must derive to x25519_pub");
}

/// POST /v1/wallet/restore/mnemonic must also return x25519_sk_hex.
#[tokio::test]
async fn wallet_restore_mnemonic_returns_x25519_sk() {
    let ctx = make_test_ctx().await.unwrap();

    // First create a wallet to get valid mnemonic words
    let (_, create_body) =
        post_json(&ctx.app, "/v1/wallet/create", serde_json::json!({})).await;
    let words: Vec<String> = create_body["mnemonic_words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let mnemonic = words.join(" ");
    let original_sk = create_body["x25519_sk_hex"].as_str().unwrap().to_string();
    let original_pk = create_body["x25519_pub_hex"].as_str().unwrap().to_string();
    println!("=== Created wallet with mnemonic ===");
    println!("Mnemonic: {mnemonic}");
    println!("Original x25519_sk_hex: {original_sk}");
    println!("Original x25519_pub_hex: {original_pk}");

    // Restore from mnemonic
    let (status, body) = post_json(
        &ctx.app,
        "/v1/wallet/restore/mnemonic",
        serde_json::json!({ "mnemonic": mnemonic }),
    )
    .await;
    println!("\n=== POST /v1/wallet/restore/mnemonic ===");
    println!("Status: {status}");
    println!("Body: {}", serde_json::to_string_pretty(&body).unwrap());

    assert_eq!(status.as_u16(), 200);

    let sk = body["x25519_sk_hex"].as_str().expect("x25519_sk_hex missing");
    let pk = body["x25519_pub_hex"].as_str().expect("x25519_pub_hex missing");
    println!("Restored x25519_sk_hex: {sk}");
    println!("Restored x25519_pub_hex: {pk}");

    // Must match original
    assert_eq!(sk, original_sk, "restored sk must match original");
    assert_eq!(pk, original_pk, "restored pk must match original");

    // Verify sk derives to pk
    let sk_bytes: [u8; 32] = hex::decode(sk).unwrap().try_into().unwrap();
    let derived_pk = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(sk_bytes));
    let derived_pk_hex = hex::encode(derived_pk.as_bytes());
    assert_eq!(derived_pk_hex, pk, "x25519_sk must derive to x25519_pub");
}

/// POST /v1/wallet/restore/private-key must also return x25519_sk_hex.
#[tokio::test]
async fn wallet_restore_private_key_returns_x25519_sk() {
    let ctx = make_test_ctx().await.unwrap();

    // First create a wallet to get a valid private key
    let (_, create_body) =
        post_json(&ctx.app, "/v1/wallet/create", serde_json::json!({})).await;
    let priv_hex = create_body["private_key_hex"].as_str().unwrap().to_string();
    let original_sk = create_body["x25519_sk_hex"].as_str().unwrap().to_string();
    let original_pk = create_body["x25519_pub_hex"].as_str().unwrap().to_string();
    println!("=== Created wallet ===");
    println!("private_key_hex: {priv_hex}");
    println!("Original x25519_sk_hex: {original_sk}");
    println!("Original x25519_pub_hex: {original_pk}");

    // Restore from private key
    let (status, body) = post_json(
        &ctx.app,
        "/v1/wallet/restore/private-key",
        serde_json::json!({ "private_key_hex": priv_hex }),
    )
    .await;
    println!("\n=== POST /v1/wallet/restore/private-key ===");
    println!("Status: {status}");
    println!("Body: {}", serde_json::to_string_pretty(&body).unwrap());

    assert_eq!(status.as_u16(), 200);

    let sk = body["x25519_sk_hex"].as_str().expect("x25519_sk_hex missing");
    let pk = body["x25519_pub_hex"].as_str().expect("x25519_pub_hex missing");
    println!("Restored x25519_sk_hex: {sk}");
    println!("Restored x25519_pub_hex: {pk}");

    // Must match original
    assert_eq!(sk, original_sk, "restored sk must match original");
    assert_eq!(pk, original_pk, "restored pk must match original");
}
