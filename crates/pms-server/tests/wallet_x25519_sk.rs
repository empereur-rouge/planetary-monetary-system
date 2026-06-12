use pms_testkit::{make_test_ctx, post_json, post_json_admin, post_json_remote};

/// Dev admin token configured by `config.dev.toml` (`env:PMS_ADMIN_TOKEN_DEV`).
/// Set before building the test app so `resolve_admin_token` picks it up.
const ADMIN_TOKEN: &str = "super-token-dev-123";

fn set_admin_token_env() {
    // SAFETY: tests are single-threaded per process for env mutation here; the
    // value is deterministic and only read at app construction.
    unsafe {
        std::env::set_var("PMS_ADMIN_TOKEN_DEV", ADMIN_TOKEN);
    }
}

/// POST /v1/wallet/create must return x25519_sk_hex that is consistent
/// with x25519_pub_hex (i.e. derived from the same ECDSA private key).
#[tokio::test]
async fn wallet_create_returns_x25519_sk() {
    set_admin_token_env();
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

/// AUDIT H-5 (v0.9.1): the restore endpoints moved to `/admin/wallet/restore/*`
/// and are gated by the admin credential. A call WITHOUT the admin token must
/// be rejected (401), even though it carries a valid mnemonic — the secret must
/// not be processed on an unauthenticated request.
#[tokio::test]
async fn wallet_restore_mnemonic_requires_admin() {
    set_admin_token_env();
    let ctx = make_test_ctx().await.unwrap();

    // Get a valid mnemonic from create (create stays API-key/public).
    let (_, create_body) =
        post_json(&ctx.app, "/v1/wallet/create", serde_json::json!({})).await;
    let words: Vec<String> = create_body["mnemonic_words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let mnemonic = words.join(" ");

    // 1) Remote caller, no admin token → 401 (the gate fires before the secret
    //    is touched). Uses a non-loopback IP because require_local_or_admin
    //    short-circuits on loopback (dev convenience).
    let (status_noauth, body_noauth) = post_json_remote(
        &ctx.app,
        "/admin/wallet/restore/mnemonic",
        serde_json::json!({ "mnemonic": mnemonic }),
    )
    .await;
    println!("=== restore/mnemonic remote WITHOUT admin token ===");
    println!("Status: {status_noauth}");
    println!("Body: {}", serde_json::to_string_pretty(&body_noauth).unwrap());
    assert_eq!(
        status_noauth.as_u16(),
        401,
        "remote restore without admin token must be 401"
    );

    // 2) Old path must no longer exist (404), proving no API-key bypass remains.
    let (status_oldpath, _) = post_json(
        &ctx.app,
        "/v1/wallet/restore/mnemonic",
        serde_json::json!({ "mnemonic": mnemonic }),
    )
    .await;
    println!("=== old path /v1/wallet/restore/mnemonic ===");
    println!("Status: {status_oldpath}");
    assert_eq!(
        status_oldpath.as_u16(),
        404,
        "old /v1 restore path must be gone"
    );
}

/// POST /admin/wallet/restore/mnemonic with the admin token must succeed and
/// return x25519_sk_hex consistent with the original wallet.
#[tokio::test]
async fn wallet_restore_mnemonic_returns_x25519_sk() {
    set_admin_token_env();
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

    // Restore from mnemonic — WITH admin token.
    let (status, body) = post_json_admin(
        &ctx.app,
        "/admin/wallet/restore/mnemonic",
        ADMIN_TOKEN,
        serde_json::json!({ "mnemonic": mnemonic }),
    )
    .await;
    println!("\n=== POST /admin/wallet/restore/mnemonic (admin) ===");
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

/// POST /admin/wallet/restore/private-key with the admin token must succeed and
/// return x25519_sk_hex consistent with the original wallet.
#[tokio::test]
async fn wallet_restore_private_key_returns_x25519_sk() {
    set_admin_token_env();
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

    // Restore from private key — WITH admin token.
    let (status, body) = post_json_admin(
        &ctx.app,
        "/admin/wallet/restore/private-key",
        ADMIN_TOKEN,
        serde_json::json!({ "private_key_hex": priv_hex }),
    )
    .await;
    println!("\n=== POST /admin/wallet/restore/private-key (admin) ===");
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
