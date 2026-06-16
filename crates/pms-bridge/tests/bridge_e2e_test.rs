use anyhow::Result;
use pms_bridge::engine::BridgeEngine;
use pms_bridge::store::BridgeStore;
use pms_bridge::types::{
    BridgeDirection, BridgeDisableRequest, BridgeEnableRequest, BridgeTransferRequest,
};
use pms_config::LedgerDef;
use pms_ledger::LedgerManager;
use pms_storage::{DagStorage, PutResult};
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_types_transaction::{TxInput, TxOutput};
use pms_utils::compute_block_id;
use pms_wallet::utils::signing_wire::canonical_wireblock_message;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::{WireBlock, WireMeta};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::{Arc, Once};
use tempfile::tempdir;
use tokio::time::{Duration, sleep};

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

/// **Bridge-mint anti-replay (audit rang 3, B3).**
///
/// The legacy "Step 7" in `bridge_full_lifecycle` does NOT test replay: it
/// re-runs the same transfer, which forges a *new* lock and fails on
/// insufficient balance. The real attack is re-submitting a coordinator-signed
/// `BridgeMint` that reuses an already-minted `lock_block_id` — which, before
/// this fix, re-minted funds out of nothing (inflation). Here we:
///   1. run a legit transfer and capture its `lock_block_id` / `mint_block_id`;
///   2. assert the destination ledger recorded the lock as consumed in the
///      DURABLE `bridge_consumed` column family (survives restarts);
///   3. forge a SECOND BridgeMint reusing that `lock_block_id` (different nonce
///      ⇒ different block id, so it is NOT mere idempotent dedup) and assert it
///      is REJECTED with "already consumed";
///   4. assert the receiver balance did NOT double.
#[tokio::test]
async fn bridge_mint_replay_is_rejected() -> Result<()> {
    let (mgr, engine, coordinator, _dir) = setup_e2e().await?;

    let sender = Wallet::generate();
    let sender_addr = sender.get_address("8e");
    let receiver = Wallet::generate();
    let receiver_addr = receiver.get_address("8e");

    // Mint 500 PMS to sender on "main", enable the bridge, transfer 100 → nft.
    mint_on_ledger(&mgr, &coordinator, "main", &sender_addr, "500.00000000").await?;
    engine.enable_bridge(
        &BridgeEnableRequest {
            ledger_a: "main".into(),
            ledger_b: "nft".into(),
            direction: BridgeDirection::Bidirectional,
        },
        true,
        None,
    )?;

    let req = BridgeTransferRequest {
        from_ledger: "main".into(),
        to_ledger: "nft".into(),
        from_address: sender_addr.clone(),
        to_address: receiver_addr.clone(),
        amount: "100.00000000".into(),
        asset_id: None,
    };
    let resp = engine.execute_transfer(&req).await?;
    println!(
        "✅ legit transfer: lock_block_id={} mint_block_id={}",
        resp.lock_block_id, resp.mint_block_id
    );

    let nft_inst = mgr.get("nft").unwrap();
    let before: Decimal = nft_inst
        .adapter
        .utxos_by_address(&receiver_addr)
        .await
        .iter()
        .map(|(_, o)| Decimal::from_str(&o.amount).unwrap())
        .sum();
    println!("receiver balance after legit mint: {before}");
    assert_eq!(before, Decimal::from_str("100.00000000")?);

    // (2) DURABLE record: poll the on-disk `bridge_consumed` CF (written by the
    // async persist consumer in the same atomic batch as the mint block).
    let mut durable = false;
    for _ in 0..40 {
        if nft_inst.store.is_bridge_lock_consumed(&resp.lock_block_id).await? {
            durable = true;
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    println!(
        "durable bridge_consumed[{}] = {durable}",
        resp.lock_block_id
    );
    assert!(
        durable,
        "the source lock must be recorded in the durable bridge_consumed CF after the mint"
    );

    // (3) REPLAY: forge a 2nd BridgeMint reusing the SAME lock_block_id.
    let nft_meta = WireMeta {
        network_id: nft_inst.def.network_id.clone(),
        protocol_version: nft_inst.def.protocol_version,
    };
    let tips = nft_inst.adapter.top_tips(1).await?;
    let parents = if tips.is_empty() {
        vec![nft_inst.store.all_block_ids().await?[0].clone()]
    } else {
        vec![tips[0].clone()]
    };
    let replay_payload = PayloadEnvelope::Plain(PlainPayload::BridgeMint {
        outputs: vec![TxOutput::new(
            receiver_addr.clone(),
            "100.00000000".to_string(),
            None,
        )],
        lock_block_id: resp.lock_block_id.clone(),
        source_ledger_id: "main".to_string(),
    });
    // nonce 999 ⇒ a different block id than the original mint: this is a genuine
    // replay, not the idempotent same-block dedup path.
    let replay_wb = forge_signed_wire_block(parents, &nft_meta, &coordinator, 999, Some(replay_payload));
    println!(
        "replay block id={} (original mint id={})",
        replay_wb.id, resp.mint_block_id
    );
    assert_ne!(
        replay_wb.id, resp.mint_block_id,
        "replay must be a DIFFERENT block id (else it's just idempotent dedup, not a replay)"
    );

    let replay_res = nft_inst.adapter.persist_block(&replay_wb).await?;
    println!("replay persist result: {replay_res:?}");
    match &replay_res {
        PutResult::Rejected(r) => {
            assert!(
                r.contains("already consumed"),
                "replay must be rejected as an already-consumed bridge lock, got: {r}"
            );
            println!("✅ replayed BridgeMint rejected: {r}");
        }
        other => panic!("replayed BridgeMint MUST be rejected, got {other:?}"),
    }

    // (4) No inflation: the receiver balance must be unchanged (still 100).
    sleep(Duration::from_millis(100)).await;
    let after: Decimal = nft_inst
        .adapter
        .utxos_by_address(&receiver_addr)
        .await
        .iter()
        .map(|(_, o)| Decimal::from_str(&o.amount).unwrap())
        .sum();
    println!("receiver balance after replay attempt: {after}");
    assert_eq!(
        after,
        Decimal::from_str("100.00000000")?,
        "a replayed BridgeMint must NOT inflate the receiver balance"
    );

    Ok(())
}

/// **Bridge-mint cross-ledger reconciliation (audit rang 3, B3).**
///
/// A `BridgeMint` must EXACTLY back a real source `BridgeLock`
/// (amount / asset / recipient) — otherwise a coordinator bug or compromise
/// could mint more than was locked (inflation) or to the wrong recipient
/// (theft). We forge an UNCONSUMED `BridgeLock` on `main`, then submit several
/// mismatched `BridgeMint`s on `nft` referencing it — each must be rejected —
/// and finally a correct one, which is accepted. Using an unconsumed lock
/// isolates the reconciliation guard from the anti-replay guard (which only
/// fires once a lock has actually been minted).
#[tokio::test]
async fn bridge_mint_reconciliation_rejects_mismatches() -> Result<()> {
    let (mgr, engine, coordinator, _dir) = setup_e2e().await?;

    let sender = Wallet::generate();
    let sender_addr = sender.get_address("8e");
    let receiver = Wallet::generate();
    let receiver_addr = receiver.get_address("8e");
    let attacker_addr = Wallet::generate().get_address("8e");

    mint_on_ledger(&mgr, &coordinator, "main", &sender_addr, "500.00000000").await?;
    engine.enable_bridge(
        &BridgeEnableRequest {
            ledger_a: "main".into(),
            ledger_b: "nft".into(),
            direction: BridgeDirection::Bidirectional,
        },
        true,
        None,
    )?;

    let main_inst = mgr.get("main").unwrap();
    let nft_inst = mgr.get("nft").unwrap();

    // ── Forge + persist an UNCONSUMED BridgeLock on main: 100 PMS → nft/receiver.
    let utxos = main_inst.adapter.utxos_by_address(&sender_addr).await;
    assert!(!utxos.is_empty(), "sender must have a UTXO to lock");
    let inputs: Vec<TxInput> = utxos
        .iter()
        .map(|(oid, _)| TxInput { out: oid.clone() })
        .collect();
    let lock_payload = PayloadEnvelope::Plain(PlainPayload::BridgeLock {
        inputs,
        amount: "100.00000000".to_string(),
        asset_id: None,
        dest_ledger_id: "nft".to_string(),
        dest_address: receiver_addr.clone(),
    });
    let main_meta = WireMeta {
        network_id: main_inst.def.network_id.clone(),
        protocol_version: main_inst.def.protocol_version,
    };
    let main_tips = main_inst.adapter.top_tips(1).await?;
    let main_parents = if main_tips.is_empty() {
        vec![main_inst.store.all_block_ids().await?[0].clone()]
    } else {
        vec![main_tips[0].clone()]
    };
    let lock_wb = forge_signed_wire_block(main_parents, &main_meta, &coordinator, 1, Some(lock_payload));
    let lock_block_id = lock_wb.id.clone();
    let lock_res = main_inst.adapter.persist_block(&lock_wb).await?;
    println!("BridgeLock persist: {lock_res:?} (id={lock_block_id})");
    assert!(
        matches!(lock_res, PutResult::Inserted),
        "BridgeLock must persist, got {lock_res:?}"
    );

    // Forge a BridgeMint on nft reusing `lock_id` (fresh nonce ⇒ distinct id).
    let nft_meta = WireMeta {
        network_id: nft_inst.def.network_id.clone(),
        protocol_version: nft_inst.def.protocol_version,
    };
    let nft_tips = nft_inst.adapter.top_tips(1).await?;
    let nft_parents = if nft_tips.is_empty() {
        vec![nft_inst.store.all_block_ids().await?[0].clone()]
    } else {
        vec![nft_tips[0].clone()]
    };
    let forge_mint = |nonce: u64, addr: &str, amount: &str, lock_id: &str| -> WireBlock {
        let payload = PayloadEnvelope::Plain(PlainPayload::BridgeMint {
            outputs: vec![TxOutput::new(addr.to_string(), amount.to_string(), None)],
            lock_block_id: lock_id.to_string(),
            source_ledger_id: "main".to_string(),
        });
        forge_signed_wire_block(nft_parents.clone(), &nft_meta, &coordinator, nonce, Some(payload))
    };

    // ── (1) WRONG AMOUNT (999 vs locked 100) → rejected (inflation guard).
    let r1 = nft_inst
        .adapter
        .persist_block(&forge_mint(101, &receiver_addr, "999.00000000", &lock_block_id))
        .await?;
    println!("wrong-amount mint: {r1:?}");
    assert!(
        matches!(&r1, PutResult::Rejected(m) if m.contains("inflation guard")),
        "wrong amount must be rejected (inflation guard), got {r1:?}"
    );

    // ── (2) WRONG RECIPIENT (right amount, attacker address) → rejected.
    let r2 = nft_inst
        .adapter
        .persist_block(&forge_mint(102, &attacker_addr, "100.00000000", &lock_block_id))
        .await?;
    println!("wrong-recipient mint: {r2:?}");
    assert!(
        matches!(&r2, PutResult::Rejected(m) if m.contains("dest_address")),
        "wrong recipient must be rejected (dest_address), got {r2:?}"
    );

    // ── (3) UNKNOWN LOCK (right amount/recipient, fabricated lock id) → rejected.
    let fake_lock = "0".repeat(64);
    let r3 = nft_inst
        .adapter
        .persist_block(&forge_mint(103, &receiver_addr, "100.00000000", &fake_lock))
        .await?;
    println!("unknown-lock mint: {r3:?}");
    assert!(
        matches!(&r3, PutResult::Rejected(m) if m.contains("unknown lock")),
        "unknown lock must be rejected, got {r3:?}"
    );

    // ── (3-bis) WRONG DESTINATION LEDGER (audit rang 3, B3 — finding #6). The
    // lock is destined for "nft"; applying it on "main" (right amount/recipient)
    // must be rejected. Otherwise a single source lock could be minted once PER
    // ledger, because the `bridge_consumed` anti-replay marker is per-destination
    // -ledger (prefix-scoped CF).
    let main_tips2 = main_inst.adapter.top_tips(1).await?;
    let main_parents2 = if main_tips2.is_empty() {
        vec![main_inst.store.all_block_ids().await?[0].clone()]
    } else {
        vec![main_tips2[0].clone()]
    };
    let wrong_ledger_payload = PayloadEnvelope::Plain(PlainPayload::BridgeMint {
        outputs: vec![TxOutput::new(
            receiver_addr.clone(),
            "100.00000000".to_string(),
            None,
        )],
        lock_block_id: lock_block_id.clone(),
        source_ledger_id: "main".to_string(),
    });
    let wrong_ledger_wb =
        forge_signed_wire_block(main_parents2, &main_meta, &coordinator, 107, Some(wrong_ledger_payload));
    let r35 = main_inst.adapter.persist_block(&wrong_ledger_wb).await?;
    println!("wrong-dest-ledger mint (applied on main): {r35:?}");
    assert!(
        matches!(&r35, PutResult::Rejected(m) if m.contains("destined for")),
        "mint on the wrong destination ledger must be rejected, got {r35:?}"
    );

    // The lock is STILL unconsumed (all 4 rejected before the commit-point claim).
    // ── (4) CORRECT mint → accepted, receiver credited 100.
    let r4 = nft_inst
        .adapter
        .persist_block(&forge_mint(104, &receiver_addr, "100.00000000", &lock_block_id))
        .await?;
    println!("correct mint: {r4:?}");
    assert!(
        matches!(r4, PutResult::Inserted),
        "correct mint must be accepted, got {r4:?}"
    );
    sleep(Duration::from_millis(50)).await;
    let bal: Decimal = nft_inst
        .adapter
        .utxos_by_address(&receiver_addr)
        .await
        .iter()
        .map(|(_, o)| Decimal::from_str(&o.amount).unwrap())
        .sum();
    println!("receiver balance after correct mint: {bal}");
    assert_eq!(bal, Decimal::from_str("100.00000000")?);

    // ── (5) Now the lock is consumed → replay rejected by the anti-replay guard.
    let r5 = nft_inst
        .adapter
        .persist_block(&forge_mint(105, &receiver_addr, "100.00000000", &lock_block_id))
        .await?;
    println!("replay-after-consume mint: {r5:?}");
    assert!(
        matches!(&r5, PutResult::Rejected(m) if m.contains("already consumed")),
        "replay of a consumed lock must be rejected, got {r5:?}"
    );

    Ok(())
}

/// **Bridge-mint MULTI-OUTPUT split-theft / inflation (audit rang 3, B3).**
///
/// The single-output test (`bridge_mint_reconciliation_rejects_mismatches`) does
/// not exercise the crafted multi-output attacks that `reconcile_bridge_mint`'s
/// per-output loop + SUM check are specifically meant to stop:
///
///   - **Split theft**: outputs sum to EXACTLY the locked amount, one goes to the
///     legit recipient, the rest are diverted to an attacker address. A naive
///     "outputs[0] matches" check would pass this. Must be REJECTED (every output
///     address must equal `dest_address`).
///   - **Inflation by extra output**: a correct full-amount output to the
///     recipient PLUS an extra output (to anyone). Sum exceeds the lock → must be
///     REJECTED (inflation guard).
///   - **Empty outputs**: a BridgeMint with zero outputs claiming a lock. Must be
///     REJECTED (no outputs).
///
/// Each attack reuses the SAME unconsumed lock; because every variant is rejected
/// BEFORE the commit-point claim, the lock survives and a final correct mint still
/// succeeds — proving the guard rejects fraud without bricking the legit path.
#[tokio::test]
async fn bridge_mint_multi_output_split_and_inflation_rejected() -> Result<()> {
    let (mgr, engine, coordinator, _dir) = setup_e2e().await?;

    let sender = Wallet::generate();
    let sender_addr = sender.get_address("8e");
    let receiver = Wallet::generate();
    let receiver_addr = receiver.get_address("8e");
    let attacker_addr = Wallet::generate().get_address("8e");

    mint_on_ledger(&mgr, &coordinator, "main", &sender_addr, "500.00000000").await?;
    engine.enable_bridge(
        &BridgeEnableRequest {
            ledger_a: "main".into(),
            ledger_b: "nft".into(),
            direction: BridgeDirection::Bidirectional,
        },
        true,
        None,
    )?;

    let main_inst = mgr.get("main").unwrap();
    let nft_inst = mgr.get("nft").unwrap();

    // Forge + persist an UNCONSUMED BridgeLock on main: 100 PMS → nft/receiver.
    let utxos = main_inst.adapter.utxos_by_address(&sender_addr).await;
    assert!(!utxos.is_empty(), "sender must have a UTXO to lock");
    let inputs: Vec<TxInput> = utxos
        .iter()
        .map(|(oid, _)| TxInput { out: oid.clone() })
        .collect();
    let lock_payload = PayloadEnvelope::Plain(PlainPayload::BridgeLock {
        inputs,
        amount: "100.00000000".to_string(),
        asset_id: None,
        dest_ledger_id: "nft".to_string(),
        dest_address: receiver_addr.clone(),
    });
    let main_meta = WireMeta {
        network_id: main_inst.def.network_id.clone(),
        protocol_version: main_inst.def.protocol_version,
    };
    let main_tips = main_inst.adapter.top_tips(1).await?;
    let main_parents = if main_tips.is_empty() {
        vec![main_inst.store.all_block_ids().await?[0].clone()]
    } else {
        vec![main_tips[0].clone()]
    };
    let lock_wb =
        forge_signed_wire_block(main_parents, &main_meta, &coordinator, 1, Some(lock_payload));
    let lock_block_id = lock_wb.id.clone();
    let lock_res = main_inst.adapter.persist_block(&lock_wb).await?;
    println!("BridgeLock persist: {lock_res:?} (id={lock_block_id})");
    assert!(matches!(lock_res, PutResult::Inserted));

    let nft_meta = WireMeta {
        network_id: nft_inst.def.network_id.clone(),
        protocol_version: nft_inst.def.protocol_version,
    };
    let nft_tips = nft_inst.adapter.top_tips(1).await?;
    let nft_parents = if nft_tips.is_empty() {
        vec![nft_inst.store.all_block_ids().await?[0].clone()]
    } else {
        vec![nft_tips[0].clone()]
    };
    let forge_multi = |nonce: u64, outputs: Vec<TxOutput>| -> WireBlock {
        let payload = PayloadEnvelope::Plain(PlainPayload::BridgeMint {
            outputs,
            lock_block_id: lock_block_id.clone(),
            source_ledger_id: "main".to_string(),
        });
        forge_signed_wire_block(nft_parents.clone(), &nft_meta, &coordinator, nonce, Some(payload))
    };

    // ── (A) SPLIT THEFT: 60 → receiver + 40 → attacker. Sum == 100 (== lock).
    // A naive "first output matches" check would pass this; the per-output
    // address check must reject it on the attacker output.
    let split = forge_multi(
        201,
        vec![
            TxOutput::new(receiver_addr.clone(), "60.00000000".to_string(), None),
            TxOutput::new(attacker_addr.clone(), "40.00000000".to_string(), None),
        ],
    );
    let ra = nft_inst.adapter.persist_block(&split).await?;
    println!("split-theft mint (60→receiver, 40→attacker, sum=100): {ra:?}");
    assert!(
        matches!(&ra, PutResult::Rejected(m) if m.contains("dest_address")),
        "split-theft must be rejected on the attacker output (dest_address), got {ra:?}"
    );

    // ── (B) INFLATION via extra output: 100 → receiver + 50 → receiver. Sum=150.
    // Both outputs go to the legit recipient, but the total exceeds the lock.
    let inflate = forge_multi(
        202,
        vec![
            TxOutput::new(receiver_addr.clone(), "100.00000000".to_string(), None),
            TxOutput::new(receiver_addr.clone(), "50.00000000".to_string(), None),
        ],
    );
    let rb = nft_inst.adapter.persist_block(&inflate).await?;
    println!("inflation mint (100+50 → receiver, sum=150 vs lock 100): {rb:?}");
    assert!(
        matches!(&rb, PutResult::Rejected(m) if m.contains("inflation guard")),
        "extra-output inflation must be rejected (inflation guard), got {rb:?}"
    );

    // ── (C) EMPTY outputs claiming the lock → rejected. Two layers can catch
    // this: the payload-authority gate ("at least one output required") runs
    // BEFORE reconciliation, and `reconcile_bridge_mint`'s own `outputs.is_empty()`
    // guard ("bridge mint has no outputs") is defense-in-depth behind it. Either
    // rejection proves an empty mint can never create funds / consume a lock.
    let empty = forge_multi(203, vec![]);
    let rc = nft_inst.adapter.persist_block(&empty).await?;
    println!("empty-output mint: {rc:?}");
    assert!(
        matches!(&rc, PutResult::Rejected(m)
            if m.contains("at least one output required") || m.contains("no outputs")),
        "empty-output mint must be rejected (authority gate or reconciliation guard), got {rc:?}"
    );

    // ── (D) The lock is STILL unconsumed (all rejected pre-commit). A correct
    // single-output full-amount mint must still succeed → receiver credited 100.
    let ok = forge_multi(
        204,
        vec![TxOutput::new(receiver_addr.clone(), "100.00000000".to_string(), None)],
    );
    let rd = nft_inst.adapter.persist_block(&ok).await?;
    println!("correct mint after rejected attacks: {rd:?}");
    assert!(
        matches!(rd, PutResult::Inserted),
        "correct mint must still succeed (fraud rejected without bricking the lock), got {rd:?}"
    );

    sleep(Duration::from_millis(50)).await;
    let recv_bal: Decimal = nft_inst
        .adapter
        .utxos_by_address(&receiver_addr)
        .await
        .iter()
        .map(|(_, o)| Decimal::from_str(&o.amount).unwrap())
        .sum();
    let atk_bal: Decimal = nft_inst
        .adapter
        .utxos_by_address(&attacker_addr)
        .await
        .iter()
        .map(|(_, o)| Decimal::from_str(&o.amount).unwrap())
        .sum();
    println!("final receiver balance: {recv_bal} | attacker balance: {atk_bal}");
    assert_eq!(
        recv_bal,
        Decimal::from_str("100.00000000")?,
        "receiver must hold EXACTLY the locked 100 (no inflation, no double credit)"
    );
    assert_eq!(
        atk_bal,
        Decimal::ZERO,
        "attacker must never receive any funds from a split-theft mint"
    );

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
