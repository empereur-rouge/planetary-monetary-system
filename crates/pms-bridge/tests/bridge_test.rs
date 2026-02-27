use anyhow::Result;
use pms_bridge::auth::BridgeAuth;
use pms_bridge::store::BridgeStore;
use pms_bridge::types::{BridgeDirection, BridgeLink};
use pms_config::LedgerDef;
use pms_ledger::LedgerManager;
use std::sync::Arc;
use tempfile::tempdir;

/// Helper: crée un Settings minimal avec 2 ledgers.
fn test_settings(db_path: &str) -> pms_config::Settings {
    pms_config::Settings {
        rocks: pms_config::Rocks {
            path: db_path.to_string(),
            prefix: "test".into(),
            tip_limit: 100,
            max_dag_blocks: 0,
            max_spent_outpoints: 0,
            checkpoint_interval_secs: None,
        },
        network: pms_config::Network {
            mode: pms_config::NetworkMode::Dev,
            network_id: "pms-test".into(),
            protocol_version: 1,
            symbol: None,
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
            api_keys_file: None,
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
            fee_tiers: vec![],
            treasury_fee_percent: 35,
            coordinator_fee_percent: 65,
            fee_distribution: None,
            mint_fee_base: None,
            mint_fee_ratio: None,
            token_creation_fee: None,
            nft_mint_fee: None,
            nft_fee_exempt_types: vec![],
            block_reward: "0.0".into(),
            annual_inflation_percent: 0.0,
            treasury_reward_percent: 0,
            creator_reward_percent: 0,
            burn_percent: 0,
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
                owner_pubkey: None, // admin-owned
                symbol: None,
            },
            LedgerDef {
                id: "nft".into(),
                network_id: "pms-nft".into(),
                prefix: "nft".into(),
                protocol_version: 1,
                tip_limit: Some(50),
                fees: None,
                validation: None,
                owner_pubkey: Some("owner_pk_nft".into()),
                symbol: None,
            },
        ],
    }
}

/// Helper: bootstrap and return (LedgerManager, BridgeStore).
async fn setup() -> Result<(Arc<LedgerManager>, BridgeStore, tempfile::TempDir)> {
    let dir = tempdir()?;
    let db_path = dir.path().join("bridge-test-db");
    std::fs::create_dir_all(&db_path)?;

    let settings = test_settings(db_path.to_str().unwrap());
    let mgr = Arc::new(LedgerManager::bootstrap(&settings).await?);

    let default = mgr.default_ledger().expect("default ledger");
    let store = BridgeStore::new(default.store.clone());

    Ok((mgr, store, dir))
}

// ═══════════════════════════════════════════════════════════════════════════
// TYPES TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn storage_key_is_sorted() {
    assert_eq!(BridgeLink::storage_key("main", "nft"), "main:nft");
    assert_eq!(BridgeLink::storage_key("nft", "main"), "main:nft");
    assert_eq!(BridgeLink::storage_key("aaa", "zzz"), "aaa:zzz");
    assert_eq!(BridgeLink::storage_key("zzz", "aaa"), "aaa:zzz");
}

#[test]
fn bridge_link_allows_transfer_bidirectional() {
    let link = BridgeLink {
        ledger_a: "alpha".into(),
        ledger_b: "beta".into(),
        direction: BridgeDirection::Bidirectional,
        enabled: true,
        created_at: 0,
        disabled_at: None,
        authorized_by: vec![],
    };

    assert!(link.allows_transfer("alpha", "beta"));
    assert!(link.allows_transfer("beta", "alpha"));
    assert!(!link.allows_transfer("alpha", "gamma"));
}

#[test]
fn bridge_link_allows_transfer_directional() {
    let link_atob = BridgeLink {
        ledger_a: "main".into(),
        ledger_b: "nft".into(),
        direction: BridgeDirection::AtoB,
        enabled: true,
        created_at: 0,
        disabled_at: None,
        authorized_by: vec![],
    };

    assert!(link_atob.allows_transfer("main", "nft"));
    assert!(!link_atob.allows_transfer("nft", "main"));

    let link_btoa = BridgeLink {
        ledger_a: "main".into(),
        ledger_b: "nft".into(),
        direction: BridgeDirection::BtoA,
        enabled: true,
        created_at: 0,
        disabled_at: None,
        authorized_by: vec![],
    };

    assert!(!link_btoa.allows_transfer("main", "nft"));
    assert!(link_btoa.allows_transfer("nft", "main"));
}

#[test]
fn bridge_link_disabled_blocks_transfer() {
    let link = BridgeLink {
        ledger_a: "main".into(),
        ledger_b: "nft".into(),
        direction: BridgeDirection::Bidirectional,
        enabled: false,
        created_at: 0,
        disabled_at: Some(100),
        authorized_by: vec![],
    };

    assert!(!link.allows_transfer("main", "nft"));
    assert!(!link.allows_transfer("nft", "main"));
}

// ═══════════════════════════════════════════════════════════════════════════
// AUTH TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn auth_admin_can_always_manage() {
    let main_def = LedgerDef {
        id: "main".into(),
        network_id: "pms-main".into(),
        prefix: "main".into(),
        protocol_version: 1,
        tip_limit: None,
        fees: None,
        validation: None,
        owner_pubkey: None, // admin-owned
        symbol: None,
    };
    let nft_def = LedgerDef {
        id: "nft".into(),
        network_id: "pms-nft".into(),
        prefix: "nft".into(),
        protocol_version: 1,
        tip_limit: None,
        fees: None,
        validation: None,
        owner_pubkey: Some("owner_pk".into()),
        symbol: None,
    };

    // Admin can enable/disable/transfer anything
    assert!(BridgeAuth::can_enable(&main_def, &nft_def, true, None));
    assert!(BridgeAuth::can_disable(&main_def, &nft_def, true, None));
    assert!(BridgeAuth::can_transfer(&main_def, &nft_def, true, None));
}

#[test]
fn auth_non_admin_cannot_manage_main_bridge() {
    let main_def = LedgerDef {
        id: "main".into(),
        network_id: "pms-main".into(),
        prefix: "main".into(),
        protocol_version: 1,
        tip_limit: None,
        fees: None,
        validation: None,
        owner_pubkey: None,
        symbol: None,
    };
    let nft_def = LedgerDef {
        id: "nft".into(),
        network_id: "pms-nft".into(),
        prefix: "nft".into(),
        protocol_version: 1,
        tip_limit: None,
        fees: None,
        validation: None,
        owner_pubkey: Some("owner_pk".into()),
        symbol: None,
    };

    // Non-admin cannot manage bridges involving main
    assert!(!BridgeAuth::can_enable(
        &main_def,
        &nft_def,
        false,
        Some("owner_pk")
    ));
    assert!(!BridgeAuth::can_disable(
        &main_def,
        &nft_def,
        false,
        Some("owner_pk")
    ));
    assert!(!BridgeAuth::can_transfer(
        &main_def,
        &nft_def,
        false,
        Some("owner_pk")
    ));
}

#[test]
fn auth_owner_can_manage_custom_bridges() {
    let custom_a = LedgerDef {
        id: "game".into(),
        network_id: "pms-game".into(),
        prefix: "game".into(),
        protocol_version: 1,
        tip_limit: None,
        fees: None,
        validation: None,
        owner_pubkey: Some("owner_a".into()),
        symbol: None,
    };
    let custom_b = LedgerDef {
        id: "market".into(),
        network_id: "pms-market".into(),
        prefix: "market".into(),
        protocol_version: 1,
        tip_limit: None,
        fees: None,
        validation: None,
        owner_pubkey: Some("owner_b".into()),
        symbol: None,
    };

    // Owner A can enable bridge between custom ledgers
    assert!(BridgeAuth::can_enable(
        &custom_a,
        &custom_b,
        false,
        Some("owner_a")
    ));
    // Owner B can too
    assert!(BridgeAuth::can_enable(
        &custom_a,
        &custom_b,
        false,
        Some("owner_b")
    ));
    // Random signer cannot
    assert!(!BridgeAuth::can_enable(
        &custom_a,
        &custom_b,
        false,
        Some("random_pk")
    ));
    // No signer cannot
    assert!(!BridgeAuth::can_enable(&custom_a, &custom_b, false, None));

    // Either owner can disable
    assert!(BridgeAuth::can_disable(
        &custom_a,
        &custom_b,
        false,
        Some("owner_a")
    ));
    assert!(BridgeAuth::can_disable(
        &custom_a,
        &custom_b,
        false,
        Some("owner_b")
    ));

    // Only source owner can transfer
    assert!(BridgeAuth::can_transfer(
        &custom_a,
        &custom_b,
        false,
        Some("owner_a")
    ));
    assert!(!BridgeAuth::can_transfer(
        &custom_a,
        &custom_b,
        false,
        Some("owner_b")
    ));
}

// ═══════════════════════════════════════════════════════════════════════════
// STORE TESTS (require RocksDB via LedgerManager)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn store_bridge_link_crud() -> Result<()> {
    let (_mgr, store, _dir) = setup().await?;

    // Initially no links
    let links = store.list_bridge_links()?;
    assert!(links.is_empty());

    // Not found
    assert!(store.get_bridge_link("main", "nft")?.is_none());

    // Create
    let link = BridgeLink {
        ledger_a: "main".into(),
        ledger_b: "nft".into(),
        direction: BridgeDirection::Bidirectional,
        enabled: true,
        created_at: 1000,
        disabled_at: None,
        authorized_by: vec!["admin".into()],
    };
    store.set_bridge_link(&link)?;

    // Read back
    let found = store.get_bridge_link("main", "nft")?.unwrap();
    assert_eq!(found.ledger_a, "main");
    assert_eq!(found.ledger_b, "nft");
    assert!(found.enabled);

    // Also findable via reversed order (storage_key sorts)
    let found2 = store.get_bridge_link("nft", "main")?.unwrap();
    assert_eq!(found2.ledger_a, "main");

    // List
    let links = store.list_bridge_links()?;
    assert_eq!(links.len(), 1);

    // Is enabled
    assert!(store.is_bridge_enabled("main", "nft")?);
    assert!(store.is_bridge_enabled("nft", "main")?);

    Ok(())
}

#[tokio::test]
async fn store_disable_bridge_link() -> Result<()> {
    let (_mgr, store, _dir) = setup().await?;

    let link = BridgeLink {
        ledger_a: "main".into(),
        ledger_b: "nft".into(),
        direction: BridgeDirection::Bidirectional,
        enabled: true,
        created_at: 1000,
        disabled_at: None,
        authorized_by: vec![],
    };
    store.set_bridge_link(&link)?;
    assert!(store.is_bridge_enabled("main", "nft")?);

    // Disable
    store.disable_bridge_link("main", "nft")?;

    let disabled = store.get_bridge_link("main", "nft")?.unwrap();
    assert!(!disabled.enabled);
    assert!(disabled.disabled_at.is_some());

    // No longer enabled
    assert!(!store.is_bridge_enabled("main", "nft")?);

    Ok(())
}

#[tokio::test]
async fn store_anti_replay() -> Result<()> {
    let (_mgr, store, _dir) = setup().await?;

    let lock_id = "lock_block_abc123";
    let mint_id = "mint_block_def456";

    // Not consumed yet
    assert!(!store.is_bridge_lock_consumed(lock_id)?);
    assert!(store.get_bridge_mint_for_lock(lock_id)?.is_none());

    // Mark consumed
    store.mark_bridge_lock_consumed(lock_id, mint_id)?;

    // Now consumed
    assert!(store.is_bridge_lock_consumed(lock_id)?);
    assert_eq!(store.get_bridge_mint_for_lock(lock_id)?.unwrap(), mint_id);

    // Double consume fails
    let result = store.mark_bridge_lock_consumed(lock_id, "another_mint");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("already consumed"));

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// SERIALIZATION TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn bridge_link_serialization_roundtrip() {
    let link = BridgeLink {
        ledger_a: "main".into(),
        ledger_b: "nft".into(),
        direction: BridgeDirection::AtoB,
        enabled: true,
        created_at: 1707753600000,
        disabled_at: None,
        authorized_by: vec!["admin_pk".into()],
    };

    let json = serde_json::to_string(&link).unwrap();
    let deserialized: BridgeLink = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.ledger_a, "main");
    assert_eq!(deserialized.ledger_b, "nft");
    assert!(matches!(deserialized.direction, BridgeDirection::AtoB));
    assert!(deserialized.enabled);
    assert_eq!(deserialized.created_at, 1707753600000);
    assert!(deserialized.disabled_at.is_none());
    assert_eq!(deserialized.authorized_by, vec!["admin_pk"]);
}

#[test]
fn bridge_direction_serialization() {
    assert_eq!(
        serde_json::to_string(&BridgeDirection::Bidirectional).unwrap(),
        "\"Bidirectional\""
    );
    assert_eq!(
        serde_json::to_string(&BridgeDirection::AtoB).unwrap(),
        "\"AtoB\""
    );
    assert_eq!(
        serde_json::to_string(&BridgeDirection::BtoA).unwrap(),
        "\"BtoA\""
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// PAYLOAD TESTS (BridgeLock / BridgeMint)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn bridge_lock_payload_serialization() {
    use pms_types_payload::{PayloadEnvelope, PlainPayload};
    use pms_types_transaction::{OutputId, TxInput};

    let payload = PayloadEnvelope::Plain(PlainPayload::BridgeLock {
        inputs: vec![TxInput {
            out: OutputId {
                txid: "abc123".into(),
                index: 0,
            },
        }],
        amount: "100.00000000".into(),
        asset_id: None,
        dest_ledger_id: "nft".into(),
        dest_address: "8e1dest_addr".into(),
    });

    let json = serde_json::to_string(&payload).unwrap();
    assert!(json.contains("BridgeLock"));
    assert!(json.contains("100.00000000"));
    assert!(json.contains("nft"));

    let rt: PayloadEnvelope = serde_json::from_str(&json).unwrap();
    if let PayloadEnvelope::Plain(PlainPayload::BridgeLock {
        amount,
        dest_ledger_id,
        ..
    }) = rt
    {
        assert_eq!(amount, "100.00000000");
        assert_eq!(dest_ledger_id, "nft");
    } else {
        panic!("expected BridgeLock");
    }
}

#[test]
fn bridge_mint_payload_serialization() {
    use pms_types_payload::{PayloadEnvelope, PlainPayload};
    use pms_types_transaction::TxOutput;

    let payload = PayloadEnvelope::Plain(PlainPayload::BridgeMint {
        outputs: vec![TxOutput {
            address: "8e1dest_addr".into(),
            amount: "100.00000000".into(),
            asset_id: None,
        }],
        lock_block_id: "lock_abc123".into(),
        source_ledger_id: "main".into(),
    });

    let json = serde_json::to_string(&payload).unwrap();
    assert!(json.contains("BridgeMint"));
    assert!(json.contains("lock_abc123"));
    assert!(json.contains("main"));

    let rt: PayloadEnvelope = serde_json::from_str(&json).unwrap();
    if let PayloadEnvelope::Plain(PlainPayload::BridgeMint {
        lock_block_id,
        source_ledger_id,
        outputs,
    }) = rt
    {
        assert_eq!(lock_block_id, "lock_abc123");
        assert_eq!(source_ledger_id, "main");
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].amount, "100.00000000");
    } else {
        panic!("expected BridgeMint");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// NOTE: End-to-end integration tests are in bridge_e2e_test.rs
// (separate file because CoreAdapter::persist_block() validates network_id
//  against the global config on disk, requiring "pms-dev" and coordinator_public_key)
// ═══════════════════════════════════════════════════════════════════════════
