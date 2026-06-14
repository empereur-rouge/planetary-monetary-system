use anyhow::Result;
use pms_bridge::engine::BridgeEngine;
use pms_bridge::store::BridgeStore;
use pms_bridge::types::{
    BridgeDirection, BridgeDisableRequest, BridgeEnableRequest, BridgeTransferRequest,
};
use pms_config::LedgerDef;
use pms_ledger::LedgerManager;
use pms_storage::DagStorage;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_types_transaction::TxOutput;
use pms_utils::compute_block_id;
use pms_wallet::utils::signing_wire::canonical_wireblock_message;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::{WireBlock, WireMeta};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::{Arc, Once};
use tempfile::tempdir;

// ═══════════════════════════════════════════════════════════════════════════
// HELPERS
// ═══════════════════════════════════════════════════════════════════════════

/// Fixed seed for deterministic coordinator wallet.
/// All tests share the same coordinator key → no env var race condition.
const COORDINATOR_SEED: [u8; 32] = [42u8; 32];

static SET_ENV: Once = Once::new();

/// Returns a deterministic coordinator wallet (same key in every test).
fn coordinator_wallet() -> Wallet {
    let w = Wallet::from_seed(&COORDINATOR_SEED, None).expect("deterministic wallet");
    SET_ENV.call_once(|| {
        // SAFETY: Only called once across all threads via Once.
        unsafe { std::env::set_var("PMS_TEST_ADMIN_PUBKEY", w.encoded_public_key()) };
    });
    w
}

/// Forge a signed WireBlock (same as pms-testkit, inlined to avoid circular dep).
fn forge_signed_wire_block(
    parents: Vec<String>,
    meta: &WireMeta,
    wallet: &Wallet,
    nonce: u64,
    payload: Option<PayloadEnvelope>,
) -> WireBlock {
    let payload_json = payload.as_ref().and_then(|p| serde_json::to_string(p).ok());

    let mut wb = WireBlock {
        id: String::new(),
        parents,
        payload_json,
        nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: None,
    };

    wb.id = compute_block_id(
        &wb.parents,
        &wb.payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok()),
        wb.nonce,
    );

    let msg = canonical_wireblock_message(&wb);
    wb.signature_hex = wallet.sign(&msg).expect("signing must not fail in test");

    wb
}

/// Create Settings with coordinator_public_key set so Mint blocks are accepted.
fn test_settings(db_path: &str, coordinator_pk: &str) -> pms_config::Settings {
    pms_config::Settings {
        rocks: pms_config::Rocks {
            path: db_path.to_string(),
            prefix: "test".into(),
            tip_limit: 100,
            max_dag_blocks: 0,
            max_spent_outpoints: 0,
            max_utxos: 0,
            checkpoint_interval_secs: None,
            write_buffer_size_mb: 128,
            max_write_buffer_number: 3,
            block_cache_size_mb: 512,
            db_write_buffer_size_mb: 512,
            max_open_files: 512,
            auto_reindex_activity_items: false,
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
            node_identity_key_encrypted_path: None,
            strict_key_permissions: false,
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
            coordinator_public_key: Some(coordinator_pk.to_string()),
            coordinator_x25519_public_key: None,
            coordinator_tx_only: false,
            enforce_single_writer: true,
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
            annual_ceiling_percent: 10.0,
            annual_floor_percent: 0.0,
            emission_epoch_duration_sec: 86400,
            coord_shard_count: 0,
            burn_rate_bps: 0,
            gas_per_tx: None,
            gas_pool_min_balance: None,
            contract_deployment_fee: None,
            storage_fee_per_kb: None,
            dynamic_fee_enabled: false,
            target_tps: 100,
            max_fee_multiplier: 5.0,
            cross_ledger_fee_multiplier: 2.0,
            ledger_annual_fee_pms: None,
        },
        p2p: pms_config::P2pConfig {
            known_peers: String::new(),
            bind_addr: None,
            allowed_peer_ips: vec![],
            strict_whitelist: false,
            max_connections: 256,
            per_peer_queue_cap: 2_000,
            max_orphans: 2_000,
            max_inflight_requests: 10_000,
            max_parent_deps: 5_000,
            max_peer_retries: 20,
        },
        health: pms_config::HealthSettings::default(),
        reserves: Default::default(),
        // NOTE: Both ledgers must use "pms-dev" as network_id because
        // CoreAdapter::persist_block() validates against the global config
        // loaded from etc/config/config.dev.toml (network_id = "pms-dev").
        ledgers: vec![
            LedgerDef {
                id: "main".into(),
                network_id: "pms-dev".into(),
                prefix: "main".into(),
                // Must match the global config.dev.toml protocol_version (=3),
                // else CoreAdapter::persist_block rejects the Mint with "wrong
                // network_id or protocol_version" (v0.9.0 canonical validation).
                protocol_version: 3,
                tip_limit: Some(100),
                fees: None,
                validation: None,
                owner_pubkey: None,
                owner_x25519_pubkey: None,
                symbol: None,
            },
            LedgerDef {
                id: "nft".into(),
                network_id: "pms-dev".into(),
                prefix: "nft".into(),
                protocol_version: 3,
                tip_limit: Some(50),
                fees: None,
                validation: None,
                owner_pubkey: Some("owner_nft_pk".into()),
                owner_x25519_pubkey: None,
                symbol: None,
            },
        ],
    }
}

/// Bootstrap a LedgerManager + BridgeEngine with a coordinator wallet.
async fn setup_e2e() -> Result<(
    Arc<LedgerManager>,
    BridgeEngine,
    Arc<Wallet>,
    tempfile::TempDir,
)> {
    let coordinator = Arc::new(coordinator_wallet());
    let coordinator_pk = coordinator.encoded_public_key();

    let dir = tempdir()?;
    let db_path = dir.path().join("bridge-e2e");
    std::fs::create_dir_all(&db_path)?;

    let settings = test_settings(db_path.to_str().unwrap(), &coordinator_pk);
    let mgr = Arc::new(LedgerManager::bootstrap(&settings).await?);

    let default = mgr.default_ledger().expect("default ledger");
    let bridge_store = BridgeStore::new(default.store.clone());
    let engine = BridgeEngine::new(mgr.clone(), bridge_store, coordinator.clone());

    Ok((mgr, engine, coordinator, dir))
}

/// Mint funds to an address on a specific ledger.
async fn mint_on_ledger(
    mgr: &LedgerManager,
    coordinator: &Wallet,
    ledger_id: &str,
    address: &str,
    amount: &str,
) -> Result<String> {
    let instance = mgr
        .get(ledger_id)
        .ok_or_else(|| anyhow::anyhow!("ledger '{}' not found", ledger_id))?;

    let tips = instance.adapter.top_tips(1).await?;
    let parents = if tips.is_empty() {
        let ids = instance.store.all_block_ids().await?;
        vec![ids[0].clone()]
    } else {
        vec![tips[0].clone()]
    };

    let payload = PayloadEnvelope::Plain(PlainPayload::Mint {
        outputs: vec![TxOutput::new(address.to_string(), amount.to_string(), None)],
    });

    let meta = WireMeta {
        network_id: instance.def.network_id.clone(),
        protocol_version: instance.def.protocol_version,
    };

    let wb = forge_signed_wire_block(parents, &meta, coordinator, 1, Some(payload));
    let block_id = wb.id.clone();

    let res = instance.adapter.persist_block(&wb).await?;
    match res {
        pms_storage::PutResult::Inserted => Ok(block_id),
        pms_storage::PutResult::AlreadyExists => Ok(block_id),
        pms_storage::PutResult::Rejected(r) => anyhow::bail!("Mint rejected: {}", r),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// E2E TESTS
// ═══════════════════════════════════════════════════════════════════════════

/// Full bridge lifecycle: mint → enable → transfer → verify → disable → transfer refused
#[tokio::test]
async fn bridge_full_lifecycle() -> Result<()> {
    let (mgr, engine, coordinator, _dir) = setup_e2e().await?;

    let sender = Wallet::generate();
    let sender_addr = sender.get_address("8e");
    let receiver = Wallet::generate();
    let receiver_addr = receiver.get_address("8e");

    // ── Step 1: Mint 500 PMS to sender on "main" ──────────────────────
    mint_on_ledger(&mgr, &coordinator, "main", &sender_addr, "500.00000000").await?;

    // Verify UTXOs on main
    let main_inst = mgr.get("main").unwrap();
    let main_utxos = main_inst.adapter.utxos_by_address(&sender_addr).await;
    assert_eq!(main_utxos.len(), 1, "sender should have 1 UTXO on main");
    assert_eq!(main_utxos[0].1.amount, "500.00000000");

    // ── Step 2: Transfer should fail (no bridge yet) ──────────────────
    let transfer_req = BridgeTransferRequest {
        from_ledger: "main".into(),
        to_ledger: "nft".into(),
        from_address: sender_addr.clone(),
        to_address: receiver_addr.clone(),
        amount: "100.00000000".into(),
        asset_id: None,
    };

    let result = engine.execute_transfer(&transfer_req).await;
    assert!(result.is_err(), "transfer should fail without bridge");
    assert!(
        result.unwrap_err().to_string().contains("no active bridge"),
        "error should mention no active bridge"
    );

    // ── Step 3: Enable bridge main ↔ nft ─────────────────────────────
    let enable_req = BridgeEnableRequest {
        ledger_a: "main".into(),
        ledger_b: "nft".into(),
        direction: BridgeDirection::Bidirectional,
    };
    let link = engine.enable_bridge(&enable_req, true, None)?;
    assert!(link.enabled);
    assert_eq!(link.ledger_a, "main");
    assert_eq!(link.ledger_b, "nft");

    // List bridges
    let links = engine.list_bridges()?;
    assert_eq!(links.len(), 1);

    // ── Step 4: Execute transfer main → nft ──────────────────────────
    let resp = engine.execute_transfer(&transfer_req).await?;
    assert_eq!(resp.from_ledger, "main");
    assert_eq!(resp.to_ledger, "nft");
    assert_eq!(resp.amount, "100.00000000");
    assert!(!resp.lock_block_id.is_empty());
    assert!(!resp.mint_block_id.is_empty());

    // ── Step 5: Verify UTXOs ─────────────────────────────────────────
    // Sender's UTXO on main should be consumed (BridgeLock spent it)
    let main_utxos_after = main_inst.adapter.utxos_by_address(&sender_addr).await;
    assert_eq!(
        main_utxos_after.len(),
        0,
        "sender should have 0 UTXOs on main after lock (all consumed)"
    );

    // Receiver should have UTXO on nft ledger
    let nft_inst = mgr.get("nft").unwrap();
    let nft_utxos = nft_inst.adapter.utxos_by_address(&receiver_addr).await;
    assert_eq!(nft_utxos.len(), 1, "receiver should have 1 UTXO on nft");
    assert_eq!(nft_utxos[0].1.amount, "100.00000000");
    assert_eq!(nft_utxos[0].1.address, receiver_addr);

    // ── Step 6: Transfer status check ────────────────────────────────
    let status = engine.transfer_status(&resp.lock_block_id)?;
    assert_eq!(status, Some(resp.mint_block_id.clone()));

    // ── Step 7: Anti-replay — same lock can't be reused ──────────────
    // (The lock is already consumed, executing the exact same transfer
    //  would create a *new* lock since it's a new block, but the sender
    //  has no UTXOs left so it should fail with insufficient balance)
    let result2 = engine.execute_transfer(&transfer_req).await;
    assert!(
        result2.is_err(),
        "second transfer should fail (no UTXOs left)"
    );

    // ── Step 8: Disable bridge ───────────────────────────────────────
    let disable_req = BridgeDisableRequest {
        ledger_a: "main".into(),
        ledger_b: "nft".into(),
    };
    let disabled = engine.disable_bridge(&disable_req, true, None)?;
    assert!(!disabled.enabled);
    assert!(disabled.disabled_at.is_some());

    // ── Step 9: Mint more and try transfer → should fail ─────────────
    mint_on_ledger(&mgr, &coordinator, "main", &sender_addr, "200.00000000").await?;

    let transfer_req2 = BridgeTransferRequest {
        from_ledger: "main".into(),
        to_ledger: "nft".into(),
        from_address: sender_addr.clone(),
        to_address: receiver_addr.clone(),
        amount: "50.00000000".into(),
        asset_id: None,
    };
    let result3 = engine.execute_transfer(&transfer_req2).await;
    assert!(
        result3.is_err(),
        "transfer should fail when bridge is disabled"
    );
    assert!(
        result3
            .unwrap_err()
            .to_string()
            .contains("no active bridge")
    );

    // ── Step 10: Re-enable bridge and transfer succeeds ──────────────
    let enable_req2 = BridgeEnableRequest {
        ledger_a: "main".into(),
        ledger_b: "nft".into(),
        direction: BridgeDirection::Bidirectional,
    };
    let link2 = engine.enable_bridge(&enable_req2, true, None)?;
    assert!(link2.enabled);

    let resp2 = engine.execute_transfer(&transfer_req2).await?;
    assert_eq!(resp2.amount, "50.00000000");

    // Verify receiver now has 2 UTXOs on nft (100 + 50)
    let nft_utxos_final = nft_inst.adapter.utxos_by_address(&receiver_addr).await;
    assert_eq!(
        nft_utxos_final.len(),
        2,
        "receiver should have 2 UTXOs on nft"
    );

    let total: Decimal = nft_utxos_final
        .iter()
        .map(|(_, o)| Decimal::from_str(&o.amount).unwrap())
        .sum();
    assert_eq!(total, Decimal::from_str("150.00000000")?);

    Ok(())
}

/// Bridge with directional constraint (A→B only).
#[tokio::test]
async fn bridge_directional_atob() -> Result<()> {
    let (mgr, engine, coordinator, _dir) = setup_e2e().await?;

    let wallet_a = Wallet::generate();
    let addr_a = wallet_a.get_address("8e");
    let wallet_b = Wallet::generate();
    let addr_b = wallet_b.get_address("8e");

    // Mint on both ledgers
    mint_on_ledger(&mgr, &coordinator, "main", &addr_a, "100.00000000").await?;
    mint_on_ledger(&mgr, &coordinator, "nft", &addr_b, "100.00000000").await?;

    // Enable bridge main → nft only (AtoB)
    let enable = BridgeEnableRequest {
        ledger_a: "main".into(),
        ledger_b: "nft".into(),
        direction: BridgeDirection::AtoB,
    };
    engine.enable_bridge(&enable, true, None)?;

    // main → nft should work
    let fwd = BridgeTransferRequest {
        from_ledger: "main".into(),
        to_ledger: "nft".into(),
        from_address: addr_a.clone(),
        to_address: addr_b.clone(),
        amount: "50.00000000".into(),
        asset_id: None,
    };
    let resp = engine.execute_transfer(&fwd).await?;
    assert_eq!(resp.amount, "50.00000000");

    // nft → main should fail (wrong direction)
    let rev = BridgeTransferRequest {
        from_ledger: "nft".into(),
        to_ledger: "main".into(),
        from_address: addr_b.clone(),
        to_address: addr_a.clone(),
        amount: "50.00000000".into(),
        asset_id: None,
    };
    let result = engine.execute_transfer(&rev).await;
    assert!(result.is_err(), "reverse transfer should fail (AtoB only)");
    assert!(result.unwrap_err().to_string().contains("no active bridge"));

    Ok(())
}

/// Insufficient balance fails gracefully.
#[tokio::test]
async fn bridge_insufficient_balance() -> Result<()> {
    let (mgr, engine, coordinator, _dir) = setup_e2e().await?;

    let sender = Wallet::generate();
    let sender_addr = sender.get_address("8e");
    let receiver_addr = Wallet::generate().get_address("8e");

    // Mint only 10 PMS
    mint_on_ledger(&mgr, &coordinator, "main", &sender_addr, "10.00000000").await?;

    // Enable bridge
    engine.enable_bridge(
        &BridgeEnableRequest {
            ledger_a: "main".into(),
            ledger_b: "nft".into(),
            direction: BridgeDirection::Bidirectional,
        },
        true,
        None,
    )?;

    // Try to transfer 100 PMS (more than available)
    let req = BridgeTransferRequest {
        from_ledger: "main".into(),
        to_ledger: "nft".into(),
        from_address: sender_addr,
        to_address: receiver_addr,
        amount: "100.00000000".into(),
        asset_id: None,
    };

    let result = engine.execute_transfer(&req).await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("insufficient balance")
    );

    Ok(())
}

/// Multiple transfers accumulate correctly on destination.
#[tokio::test]
async fn bridge_multiple_transfers() -> Result<()> {
    let (mgr, engine, coordinator, _dir) = setup_e2e().await?;

    let sender = Wallet::generate();
    let sender_addr = sender.get_address("8e");
    let receiver_addr = Wallet::generate().get_address("8e");

    // Enable bridge
    engine.enable_bridge(
        &BridgeEnableRequest {
            ledger_a: "main".into(),
            ledger_b: "nft".into(),
            direction: BridgeDirection::Bidirectional,
        },
        true,
        None,
    )?;

    // Mint 3 separate UTXOs on main
    for _ in 0..3 {
        mint_on_ledger(&mgr, &coordinator, "main", &sender_addr, "100.00000000").await?;
    }

    let main_inst = mgr.get("main").unwrap();
    let utxos_before = main_inst.adapter.utxos_by_address(&sender_addr).await;
    assert_eq!(utxos_before.len(), 3, "should have 3 UTXOs from 3 mints");

    // Transfer 3 times (each consumes 1 UTXO)
    for _ in 0..3 {
        let req = BridgeTransferRequest {
            from_ledger: "main".into(),
            to_ledger: "nft".into(),
            from_address: sender_addr.clone(),
            to_address: receiver_addr.clone(),
            amount: "100.00000000".into(),
            asset_id: None,
        };
        engine.execute_transfer(&req).await?;
    }

    // All UTXOs consumed on main
    let main_utxos = main_inst.adapter.utxos_by_address(&sender_addr).await;
    assert_eq!(main_utxos.len(), 0);

    // 3 UTXOs on nft
    let nft_inst = mgr.get("nft").unwrap();
    let nft_utxos = nft_inst.adapter.utxos_by_address(&receiver_addr).await;
    assert_eq!(nft_utxos.len(), 3);

    let total: Decimal = nft_utxos
        .iter()
        .map(|(_, o)| Decimal::from_str(&o.amount).unwrap())
        .sum();
    assert_eq!(total, Decimal::from_str("300.00000000")?);

    Ok(())
}
