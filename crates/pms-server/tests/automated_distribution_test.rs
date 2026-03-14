use anyhow::Result;
use async_trait::async_trait;
use pms_config::{
    Address, Admin, Auth, FeePickMode, FeesSettings, Limits, Network, NetworkMode, P2pConfig,
    Rocks, SecretSettings, Settings, TreasuryWallets, ValidationSettings,
};
use pms_interface::NetDagAdapter;
use pms_server::Server;
use pms_server::api::{AppState, spawn_fee_distributor_task};
use pms_storage::rocks_store::store::RocksStore;
use pms_storage::{DagStorage, PutResult};
use pms_types_block::{Block, BlockMetadata};
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireBlock;
use rust_decimal::Decimal;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;
use tempfile::TempDir;

// MockAdapter to satisfy Server requirements and allow testing without full Core stack
struct MockAdapter {
    store: Arc<RocksStore>,
}

#[async_trait]
impl NetDagAdapter for MockAdapter {
    async fn have_block(&self, id: &str) -> bool {
        self.store.get_block(id).await.unwrap().is_some()
    }

    async fn persist_block(&self, wb: &WireBlock) -> Result<PutResult> {
        // Convert WireBlock to StoredBlock and persist
        use pms_storage::StoredBlock;
        let sb = StoredBlock {
            id: wb.id.clone(),
            parents: wb.parents.clone(),
            payload_json: wb.payload_json.clone(),
            nonce: wb.nonce,
            network_id: wb.network_id.clone(),
            protocol_version: wb.protocol_version,
            signer_pk_hex: wb.signer_pk_hex.clone(),
            signature_hex: wb.signature_hex.clone(),
            metadata: wb.metadata.clone(),
        };
        // Use inherent put_block from RocksStore
        // Note: RocksStore::put_block takes &StoredBlock
        self.store.put_block(&sb).await
    }

    async fn broadcast_block(&self, _b: &WireBlock) -> Result<()> {
        Ok(())
    }

    async fn top_tips(&self, limit: usize) -> Result<Vec<String>> {
        self.store.top_tips(limit).await
    }

    async fn get_block(&self, id: &str) -> Result<Option<WireBlock>> {
        if let Some(sb) = self.store.get_block(id).await? {
            Ok(Some(WireBlock {
                id: sb.id,
                parents: sb.parents,
                payload_json: sb.payload_json,
                nonce: sb.nonce,
                network_id: sb.network_id,
                protocol_version: sb.protocol_version,
                signer_pk_hex: sb.signer_pk_hex,
                signature_hex: sb.signature_hex,
                metadata: sb.metadata,
            }))
        } else {
            Ok(None)
        }
    }

    async fn recent_ids(&self, limit: usize) -> Result<Vec<String>> {
        self.store.recent_ids(limit).await
    }

    async fn get_blocks_by_ids(&self, _ids: &[String]) -> Result<Vec<WireBlock>> {
        Ok(vec![])
    }

    fn min_pow_leading_zero_bits(&self) -> u8 {
        0
    }

    async fn circulating_supply(&self) -> (Decimal, u64) {
        (Decimal::ZERO, 0)
    }

    async fn circulating_supply_by_asset(&self, _asset_id: Option<&str>) -> (Decimal, u64) {
        (Decimal::ZERO, 0)
    }

    async fn balance_by_address(&self, _address: &str) -> Decimal {
        Decimal::ZERO
    }

    async fn utxos_by_address(
        &self,
        _address: &str,
    ) -> Vec<(pms_types::OutputId, pms_types::TxOutput)> {
        Vec::new()
    }

    async fn add_utxo(
        &self,
        _txid: String,
        _index: u32,
        _address: String,
        _amount: String,
        _asset_id: Option<String>,
    ) {
        // No-op for mock, unless we want to verify UTXOs
    }

    async fn remove_utxo(&self, _output_id: &pms_types::OutputId) -> bool {
        false // Mock: no-op
    }

    async fn get_utxo(&self, _output_id: &pms_types::OutputId) -> Option<pms_types::TxOutput> {
        None // Mock: no UTXOs stored
    }
}

#[tokio::test]
async fn test_automated_fee_distribution() {
    // 1. Setup Temp RocksDB
    let temp_dir = TempDir::new().unwrap();
    let rocks_store_arc = Arc::new(
        RocksStore::new(temp_dir.path().to_str().unwrap(), 100, "test", None)
            .await
            .unwrap(),
    );

    // 2. Setup MockAdapter
    let adapter = Arc::new(MockAdapter {
        store: rocks_store_arc.clone(),
    });

    // 3. Setup Wallet (Coordinator)
    let node_wallet = Arc::new(Wallet::generate());
    let node_pk = node_wallet.encoded_public_key();

    // 4. Setup Config with 1s interval
    let settings = Settings {
        rocks: Rocks {
            path: temp_dir.path().to_str().unwrap().to_string(),
            prefix: "test".into(),
            tip_limit: 100,
            max_dag_blocks: 0,
            max_spent_outpoints: 0,
            max_utxos: 0,
            checkpoint_interval_secs: None,
        },
        network: Network {
            mode: NetworkMode::Dev,
            network_id: "test".into(),
            protocol_version: 1,
            symbol: None,
        },
        address: Address { hrp: "8e".into() },
        admin: Admin {
            wallet_addresses: vec![],
            signer_pubkeys: vec![],
            treasury_wallets_file: None,
        },
        client: None,
        tls: None,
        limits: Limits {
            max_body_bytes: 1000,
            request_timeout_ms: 1000,
            rate_limit_rps: 100,
            burst: 100,
        },
        auth: Auth {
            require_signed_submit: false,
            admin_api_token: None,
            allowed_ips: vec![],
            api_keys_file: None,
        },
        secrets: SecretSettings {
            node_identity_key_path: ".".into(),
            admin_wallet_file: None,
        },
        validation: ValidationSettings {
            min_pow_leading_zero_bits: 0,
            max_payload_bytes: 1000,
            min_parents_after_boot: 0, // Allow 0 parents for bootstrap
            max_parents: 8,
            require_unique_parents: false,
            forbid_self_parent: false,
            max_inputs: 10,
            max_outputs: 10,
            max_tx_bytes: 1000,
            max_fee_per_tx: 1000,
            enforce_parent_existence: false,
            enforce_fee_recipient: false,
            allowed_fee_addresses: vec![],
            coordinator_public_key: Some(node_pk.clone()),
            coordinator_x25519_public_key: None,
            coordinator_tx_only: false,
            enforce_single_writer: false, // Tests need multi-writer flexibility
        },
        fees: FeesSettings {
            epsilon: "0.0".into(),
            ratio: "0.0".into(),
            base_fee: "0.0".into(),
            mode: FeePickMode::Uniform,
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
            distribution_interval_sec: 1, // 1 second interval for test
            daily_inflation_enabled: false,
            daily_inflation_interval_sec: 86400,
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
        p2p: P2pConfig {
            known_peers: "".into(),
            bind_addr: Some("127.0.0.1:0".to_string()),
            allowed_peer_ips: vec![],
            strict_whitelist: false,
        },
        ledgers: vec![],
    };

    // 5. Create Server with MockAdapter
    let server_config = Arc::new(pms_config::ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        api_addr: "127.0.0.1:0".into(),
        tls: None,
        network: settings.network.clone(),
        auth: settings.auth.clone(),
    });

    // Correct Arguments:
    // 1. adapter: Arc<dyn NetDagAdapter>
    // 2. network_id: impl Into<String>
    // 3. protocol_version: u32
    // 4. node_wallet: Arc<Wallet>

    let srv = Server::new(
        adapter.clone(),
        "test_network".to_string(),
        1,
        node_wallet.clone(),
        &settings.p2p,
        None,
    );

    // 6. Seed Genesis Block (so we have a tip)
    let genesis_block = Block {
        id: String::new(),
        parents: vec![],
        payload: Some(PayloadEnvelope::Plain(PlainPayload::Genesis)),
        nonce: 0,
        metadata: Some(BlockMetadata::default()),
        signer_pk: None,
        signature: None,
    };
    let mut g = genesis_block.clone();
    g.id = compute_block_id(&g.parents, &g.payload, g.nonce);

    let wb = WireBlock {
        id: g.id.clone(),
        parents: g.parents,
        payload_json: serde_json::to_string(&g.payload).ok(),
        nonce: g.nonce,
        network_id: "test".to_string(),
        protocol_version: 1,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: g.metadata,
    };

    // Use MockAdapter or RocksStore to persist genesis
    adapter
        .persist_block(&wb)
        .await
        .expect("Genesis persist failed");
    rocks_store_arc
        .add_tip(&wb.id)
        .await
        .expect("Add tip failed");

    // 7. Setup AppState
    let state = AppState {
        srv: srv.clone(),
        _cfg: server_config,
        _ready: Arc::new(AtomicBool::new(true)),
        stats: Arc::new(pms_server::stats::Stats::new()),
        store: rocks_store_arc.clone(), // Concrete type
        admin_token: None,
        node_wallet: node_wallet.clone(),
        settings: Arc::new(settings.clone()),
        allowed_networks: vec![],
        treasury_wallets: TreasuryWallets::empty(),
        node_registry: pms_server::node_registry::create_registry(),
        fee_pool: pms_server::fee_pool::create_fee_pool(),
        api_key_store: pms_server::api_keys::create_api_key_store(None).unwrap(),
        ledger_mgr: None,
        ledger_id: "main".into(),
        effective_fees: Arc::new(pms_server::api_fn::tx_helpers::resolve_effective_fees(
            &settings.fees,
            None,
        )),
        activity_cache: Arc::new(pms_server::api_fn::activity::ActivityCache::new(1_000, 30)),
        tps_tracker: Arc::new(pms_economics::dynamic_fee::TpsTracker::new(60)),
    };

    // 8. Spawn Distributor
    println!("🚀 Spawning fee distributor task...");
    spawn_fee_distributor_task(state.clone());

    // 9. Inject Fees (Burn Refund)
    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("💰 Injecting fees...");
    {
        let mut pool = state.fee_pool.write().await;
        pool.add_burn_refund("test_address_123", "100.0".parse().unwrap(), None);
        assert!(pool.has_fees(), "Pool should have fees");
    }

    // 10. Wait for distribution
    println!("⏳ Waiting for distribution...");
    tokio::time::sleep(Duration::from_millis(2500)).await;

    // 11. Verify
    {
        let pool = state.fee_pool.read().await;
        assert!(
            !pool.has_fees(),
            "Fee pool should be empty after distribution!"
        );
    }

    let tips = rocks_store_arc.top_tips(10).await.unwrap();
    assert!(tips.len() >= 1);

    println!("✅ Test passed (pool drained)!");
}
