use anyhow::Result;
use pms_config::LedgerDef;
use pms_ledger::LedgerManager;
use pms_storage::DagStorage;
use tempfile::tempdir;

/// Helper: crée un Settings minimal pointant vers un tempdir.
fn test_settings(db_path: &str) -> pms_config::Settings {
    pms_config::Settings {
        rocks: pms_config::Rocks {
            path: db_path.to_string(),
            prefix: "test".into(),
            tip_limit: 100,
            checkpoint_interval_secs: None,
        },
        network: pms_config::Network {
            mode: pms_config::NetworkMode::Dev,
            network_id: "pms-test".into(),
            protocol_version: 1,
        },
        address: pms_config::Address { hrp: "8e".into() },
        admin: pms_config::Admin {
            wallet_addresses: vec![],
            signer_pubkeys: vec![],
            treasury_wallets_file: None,
        },
        client: None,
        tls: None,
        limits: pms_config::Limits {
            max_body_bytes: 1_000_000,
            request_timeout_ms: 5000,
            rate_limit_rps: 100,
            burst: 100,
        },
        auth: pms_config::Auth {
            require_signed_submit: false,
            admin_api_token: None,
            allowed_ips: vec![],
        },
        secrets: pms_config::SecretSettings {
            node_identity_key_path: ".".into(),
            admin_wallet_file: None,
        },
        validation: pms_config::ValidationSettings {
            min_pow_leading_zero_bits: 0,
            max_payload_bytes: 100_000,
            min_parents_after_boot: 0,
            max_parents: 8,
            require_unique_parents: false,
            forbid_self_parent: false,
            max_inputs: 10,
            max_outputs: 10,
            max_tx_bytes: 100_000,
            max_fee_per_tx: 1000,
            enforce_parent_existence: false,
            enforce_fee_recipient: false,
            allowed_fee_addresses: vec![],
            coordinator_public_key: None,
            coordinator_x25519_public_key: None,
            coordinator_tx_only: false,
            enforce_single_writer: false,
        },
        fees: pms_config::FeesSettings {
            epsilon: "0.0".into(),
            ratio: "0.0".into(),
            base_fee: "0.0".into(),
            mode: pms_config::FeePickMode::Uniform,
            seed: None,
            platform_address: None,
            platform_address_signature: None,
            platform_fee_ratio: "0.0".into(),
            treasury_fee_percent: 35,
            coordinator_fee_percent: 65,
            block_reward: "0.0".into(),
            annual_inflation_percent: 0.0,
            treasury_reward_percent: 0,
            creator_reward_percent: 0,
            burn_percent: 0,
            authority_public_keys: vec![],
            authority_keys_last_rotation: None,
            treasury_addresses: vec![],
            distribution_interval_sec: 600,
            daily_inflation_enabled: false,
            daily_inflation_interval_sec: 86400,
        },
        p2p: pms_config::P2pConfig {
            known_peers: String::new(),
            bind_addr: None,
            allowed_peer_ips: vec![],
            strict_whitelist: false,
        },
        ledgers: vec![
            LedgerDef {
                id: "main".into(),
                network_id: "pms-main".into(),
                prefix: "main".into(),
                protocol_version: 1,
                tip_limit: Some(100),
                fees: None,
                validation: None,
                owner_pubkey: None,
            },
            LedgerDef {
                id: "nft".into(),
                network_id: "pms-nft".into(),
                prefix: "nft".into(),
                protocol_version: 1,
                tip_limit: Some(50),
                fees: None,
                validation: None,
                owner_pubkey: None,
            },
        ],
    }
}

/// Bootstrap 2 ledgers, verify they each have their own DAG, store, UTXOs.
#[tokio::test]
async fn bootstrap_two_ledgers_isolated() -> Result<()> {
    let dir = tempdir()?;
    let db_path = dir.path().join("multi-ledger-test");
    std::fs::create_dir_all(&db_path)?;

    let settings = test_settings(db_path.to_str().unwrap());
    let mgr = LedgerManager::bootstrap(&settings).await?;

    // Both ledgers created
    assert_eq!(mgr.len(), 2);
    assert!(mgr.get("main").is_some());
    assert!(mgr.get("nft").is_some());

    let main = mgr.get("main").unwrap();
    let nft = mgr.get("nft").unwrap();

    // Each has exactly 1 block (genesis)
    assert_eq!(main.dag.len(), 1, "main ledger should have genesis");
    assert_eq!(nft.dag.len(), 1, "nft ledger should have genesis");

    // Store block IDs are independent
    let main_ids = main.store.all_block_ids().await?;
    let nft_ids = nft.store.all_block_ids().await?;
    assert_eq!(main_ids.len(), 1);
    assert_eq!(nft_ids.len(), 1);

    // Genesis IDs should be identical (same algorithm, same params)
    assert_eq!(main_ids[0], nft_ids[0], "genesis IDs should match");

    // Adapters are distinct
    let main_tips = main.adapter.top_tips(10).await?;
    let nft_tips = nft.adapter.top_tips(10).await?;
    assert_eq!(main_tips.len(), 1);
    assert_eq!(nft_tips.len(), 1);

    Ok(())
}

/// Lookup by network_id works.
#[tokio::test]
async fn lookup_by_network_id() -> Result<()> {
    let dir = tempdir()?;
    let db_path = dir.path().join("network-id-test");
    std::fs::create_dir_all(&db_path)?;

    let settings = test_settings(db_path.to_str().unwrap());
    let mgr = LedgerManager::bootstrap(&settings).await?;

    let found = mgr.get_by_network_id("pms-nft");
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, "nft");

    let not_found = mgr.get_by_network_id("unknown");
    assert!(not_found.is_none());

    Ok(())
}

/// Default ledger falls back correctly.
#[tokio::test]
async fn default_ledger_is_main() -> Result<()> {
    let dir = tempdir()?;
    let db_path = dir.path().join("default-ledger-test");
    std::fs::create_dir_all(&db_path)?;

    let settings = test_settings(db_path.to_str().unwrap());
    let mgr = LedgerManager::bootstrap(&settings).await?;

    let default = mgr.default_ledger();
    assert!(default.is_some());
    assert_eq!(default.unwrap().id, "main");

    Ok(())
}

/// Dynamic ledger creation at runtime.
#[tokio::test]
async fn add_ledger_dynamically() -> Result<()> {
    let dir = tempdir()?;
    let db_path = dir.path().join("dynamic-ledger-test");
    std::fs::create_dir_all(&db_path)?;

    let settings = test_settings(db_path.to_str().unwrap());
    let mgr = LedgerManager::bootstrap(&settings).await?;
    assert_eq!(mgr.len(), 2);

    // Dynamically add a third ledger
    let new_def = LedgerDef {
        id: "sidechain".into(),
        network_id: "pms-side".into(),
        prefix: "side".into(),
        protocol_version: 1,
        tip_limit: Some(32),
        fees: None,
        validation: None,
        owner_pubkey: None,
    };

    let instance = mgr.add_ledger(new_def).await?;
    assert_eq!(instance.id, "sidechain");
    assert_eq!(instance.dag.len(), 1, "new ledger should have genesis");
    assert_eq!(mgr.len(), 3);

    // Should be findable by ID and network_id
    assert!(mgr.get("sidechain").is_some());
    assert!(mgr.get_by_network_id("pms-side").is_some());

    // Duplicate should fail
    let dup = mgr
        .add_ledger(LedgerDef {
            id: "sidechain".into(),
            network_id: "pms-side2".into(),
            prefix: "side2".into(),
            protocol_version: 1,
            tip_limit: None,
            fees: None,
            validation: None,
            owner_pubkey: None,
        })
        .await;
    assert!(dup.is_err(), "duplicate ledger id should be rejected");

    Ok(())
}

/// list_ids and list_all return all ledgers.
#[tokio::test]
async fn list_operations() -> Result<()> {
    let dir = tempdir()?;
    let db_path = dir.path().join("list-ops-test");
    std::fs::create_dir_all(&db_path)?;

    let settings = test_settings(db_path.to_str().unwrap());
    let mgr = LedgerManager::bootstrap(&settings).await?;

    let ids = mgr.list_ids();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"main".to_string()));
    assert!(ids.contains(&"nft".to_string()));

    let all = mgr.list_all();
    assert_eq!(all.len(), 2);

    assert!(!mgr.is_empty());

    Ok(())
}
