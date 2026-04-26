use axum::Router;
use pms_config::{ServerConfig, TreasuryWallets, load_config};
use pms_core::ConcurrentDag;
use pms_interface::NetDagAdapter;
use pms_server::api::{AppState, build_api_router};
use pms_server::stats::Stats;
use pms_server::{Server, resolve_admin_token};
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use pms_types::Block;
use pms_wallet::{SignerBackend, Wallet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Helper : crée un Router complet mais utilisé en mémoire seulement.
pub async fn make_test_app() -> anyhow::Result<axum::Router> {
    let settings = load_config()?;

    // 1) RocksStore temporaire
    let tmp = tempfile::tempdir()?;
    let db_path = tmp.path().join("rocks-security");
    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            settings.rocks.tip_limit as usize,
            &settings.rocks.prefix,
            None,
            &RocksMemoryConfig::default(),
        )
        .await?,
    );
    store.ensure_schema().await?;
    store.bootstrap_once_for_production()?;

    // 2) Genesis si DB vide
    if store.all_block_ids().await?.is_empty() {
        let g = Block::genesis(pms_utils::compute_block_id);
        let meta = pms_wire::WireMeta::from(&settings);
        store.persist_genesis(&g, &meta).await?;
    }

    // 3) DAG
    let dag_loaded = ConcurrentDag::bootstrap_from_store(&*store).await?;
    let dag = Arc::new(dag_loaded);

    // 4) Adapter
    let adapter: Arc<dyn NetDagAdapter> = pms_core::CoreAdapter::new(dag.clone(), store.clone(), 0, None);

    // 5) Wallet de node pour les tests (en mémoire, pas de fichier)
    let node_wallet =
        Wallet::from_seed(&[1u8; 32], None).expect("wallet de test ne doit pas échouer");
    let node_wallet = Arc::new(node_wallet);

    // 6) Serveur
    let srv = Server::new(
        adapter,
        &settings.network.network_id,
        settings.network.protocol_version,
        node_wallet.clone(),
        &settings.p2p,
        None, // no multi-ledger in tests
    );

    // 7) ServerConfig minimal (ports pas utilisés ici)
    let cfg = Arc::new(ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        api_addr: "127.0.0.1:0".into(),
        tls: settings.tls.clone(),
        api_tls_enabled: false,
        network: settings.network.clone(),
        auth: settings.auth.clone(),
    });

    let ready = Arc::new(AtomicBool::new(true));
    let stats = Arc::new(Stats::new());

    // 8) Token admin
    let admin_token = settings
        .auth
        .admin_api_token
        .as_deref()
        .and_then(resolve_admin_token);

    // 9) AppState
    let state = AppState {
        srv,
        _cfg: cfg,
        _ready: ready,
        stats,
        contract_store: store.clone(),
        compliance_lock: Arc::new(tokio::sync::Mutex::new(())),
        coord_shard_wallets: std::sync::Arc::new(Vec::new()),
        coord_shard_round_robin: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        store,
        admin_token,
        node_wallet,
        settings: Arc::new(settings.clone()),
        allowed_networks: vec![], // Tests: allow all IPs
        treasury_wallets: TreasuryWallets::empty(),
        node_registry: pms_server::node_registry::create_registry(),
        fee_pool: pms_server::fee_pool::create_fee_pool(),
        fee_pool_registry: Arc::new(pms_server::fee_pool::FeePoolRegistry::new()),
        api_key_store: pms_server::api_keys::create_api_key_store(None).unwrap(),
        ledger_mgr: None,
        ledger_id: "main".into(),
        effective_fees: Arc::new(pms_server::api_fn::tx_helpers::resolve_effective_fees(
            &settings.fees,
            None,
        )),
        activity_cache: Arc::new(pms_server::api_fn::activity::ActivityCache::new(1_000, 30)),
        tps_tracker: Arc::new(pms_economics::dynamic_fee::TpsTracker::new(60)),
        contract_event_bus: None,
    };

    // 10) Router axum
    Ok(build_api_router(state, &settings))
}

pub struct TestCtx {
    pub app: Router,
    pub store: Arc<RocksStore>,
    pub settings: pms_config::Settings,
    pub srv: Arc<Server>,
    pub node_wallet: Arc<Wallet>,
}

/// Version test de make_test_app qui expose les dépendances.
pub async fn make_test_ctx() -> anyhow::Result<TestCtx> {
    // 0) charge config (tip_limit, hrp, etc.)
    let settings = load_config()?; // ✅ un seul load

    // 1) RocksStore temporaire
    let tmp = tempfile::tempdir()?;
    let db_path = tmp.path().join("rocks-fees");
    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            settings.rocks.tip_limit as usize,
            &settings.rocks.prefix,
            None,
            &RocksMemoryConfig::default(),
        )
        .await?,
    );
    store.ensure_schema().await?;
    store.bootstrap_once_for_production()?;

    // 2) Genesis si DB vide
    if store.all_block_ids().await?.is_empty() {
        let g = Block::genesis(pms_utils::compute_block_id);
        let meta = pms_wire::WireMeta::from(&settings);
        store.persist_genesis(&g, &meta).await?;
    }

    // 3) DAG
    let dag_loaded = ConcurrentDag::bootstrap_from_store(&*store).await?;
    let dag = Arc::new(dag_loaded);

    // 4) Adapter
    let adapter: Arc<dyn NetDagAdapter> = pms_core::CoreAdapter::new(dag.clone(), store.clone(), 0, None);

    // 5) Wallet node (en mémoire)
    let node_wallet = Arc::new(Wallet::from_seed(&[7u8; 32], None).unwrap());
    // 6) Serveur
    let srv = Server::new(
        adapter,
        &settings.network.network_id,
        settings.network.protocol_version,
        node_wallet.clone(),
        &settings.p2p,
        None,
    );

    // 7) ServerConfig minimal
    let cfg = Arc::new(ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        api_addr: "127.0.0.1:0".into(),
        tls: settings.tls.clone(),
        api_tls_enabled: false,
        network: settings.network.clone(),
        auth: settings.auth.clone(),
    });

    let ready = Arc::new(AtomicBool::new(true));
    let stats = Arc::new(Stats::new());

    // 8) Token admin
    let admin_token = settings
        .auth
        .admin_api_token
        .as_deref()
        .and_then(resolve_admin_token);

    // 9) AppState
    let state = AppState {
        srv: srv.clone(),
        _cfg: cfg,
        _ready: ready,
        stats,
        store: store.clone(),
        admin_token,
        node_wallet: node_wallet.clone(), // ✅ pour wallet_send_tx
        settings: Arc::new(settings.clone()),
        allowed_networks: vec![], // Tests: allow all IPs
        treasury_wallets: TreasuryWallets::empty(),
        node_registry: pms_server::node_registry::create_registry(),
        fee_pool: pms_server::fee_pool::create_fee_pool(),
        fee_pool_registry: Arc::new(pms_server::fee_pool::FeePoolRegistry::new()),
        api_key_store: pms_server::api_keys::create_api_key_store(None).unwrap(),
        ledger_mgr: None,
        ledger_id: "main".into(),
        effective_fees: Arc::new(pms_server::api_fn::tx_helpers::resolve_effective_fees(
            &settings.fees,
            None,
        )),
        activity_cache: Arc::new(pms_server::api_fn::activity::ActivityCache::new(1_000, 30)),
        tps_tracker: Arc::new(pms_economics::dynamic_fee::TpsTracker::new(60)),
        contract_event_bus: None,
        contract_store: store.clone(),
        compliance_lock: Arc::new(tokio::sync::Mutex::new(())),
        coord_shard_wallets: std::sync::Arc::new(Vec::new()),
        coord_shard_round_robin: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };

    // 10) Router
    let app = build_api_router(state, &settings);

    Ok(TestCtx {
        app,
        store,
        settings,
        srv,
        node_wallet,
    })
}

/// Version of make_test_ctx that allows configuring admin wallet addresses and signers
/// This is needed for fee-related tests where admin addresses must be pre-configured.
pub async fn make_test_ctx_with_admin(
    admin_wallet_addresses: Vec<String>,
    admin_signer_pubkeys: Vec<String>,
) -> anyhow::Result<TestCtx> {
    // 0) charge config (tip_limit, hrp, etc.)
    let mut settings = load_config()?;

    // Override admin wallet addresses and signer pubkeys
    settings.admin.wallet_addresses = admin_wallet_addresses;
    settings.admin.signer_pubkeys = admin_signer_pubkeys;

    // 1) RocksStore temporaire
    let tmp = tempfile::tempdir()?;
    let db_path = tmp.path().join("rocks-fees-admin");
    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            settings.rocks.tip_limit as usize,
            &settings.rocks.prefix,
            None,
            &RocksMemoryConfig::default(),
        )
        .await?,
    );
    store.ensure_schema().await?;
    store.bootstrap_once_for_production()?;

    // 2) Genesis si DB vide
    if store.all_block_ids().await?.is_empty() {
        let g = Block::genesis(pms_utils::compute_block_id);
        let meta = pms_wire::WireMeta::from(&settings);
        store.persist_genesis(&g, &meta).await?;
    }

    // 3) DAG
    let dag_loaded = ConcurrentDag::bootstrap_from_store(&*store).await?;
    let dag = Arc::new(dag_loaded);

    // 4) Adapter
    let adapter: Arc<dyn NetDagAdapter> = pms_core::CoreAdapter::new(dag.clone(), store.clone(), 0, None);

    // 5) Wallet node (en mémoire) - use same seed as make_test_ctx
    let node_wallet = Arc::new(Wallet::from_seed(&[7u8; 32], None).unwrap());

    // 6) Serveur
    let srv = Server::new(
        adapter,
        &settings.network.network_id,
        settings.network.protocol_version,
        node_wallet.clone(),
        &settings.p2p,
        None,
    );

    // 7) ServerConfig minimal
    let cfg = Arc::new(ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        api_addr: "127.0.0.1:0".into(),
        tls: settings.tls.clone(),
        api_tls_enabled: false,
        network: settings.network.clone(),
        auth: settings.auth.clone(),
    });

    let ready = Arc::new(AtomicBool::new(true));
    let stats = Arc::new(Stats::new());

    // 8) Token admin
    let admin_token = settings
        .auth
        .admin_api_token
        .as_deref()
        .and_then(resolve_admin_token);

    // 9) AppState
    let state = AppState {
        srv: srv.clone(),
        _cfg: cfg,
        _ready: ready,
        stats,
        store: store.clone(),
        admin_token,
        node_wallet: node_wallet.clone(),
        settings: Arc::new(settings.clone()),
        allowed_networks: vec![], // Tests: allow all IPs
        treasury_wallets: TreasuryWallets::empty(),
        node_registry: pms_server::node_registry::create_registry(),
        fee_pool: pms_server::fee_pool::create_fee_pool(),
        fee_pool_registry: Arc::new(pms_server::fee_pool::FeePoolRegistry::new()),
        api_key_store: pms_server::api_keys::create_api_key_store(None).unwrap(),
        ledger_mgr: None,
        ledger_id: "main".into(),
        effective_fees: Arc::new(pms_server::api_fn::tx_helpers::resolve_effective_fees(
            &settings.fees,
            None,
        )),
        activity_cache: Arc::new(pms_server::api_fn::activity::ActivityCache::new(1_000, 30)),
        tps_tracker: Arc::new(pms_economics::dynamic_fee::TpsTracker::new(60)),
        contract_event_bus: None,
        contract_store: store.clone(),
        compliance_lock: Arc::new(tokio::sync::Mutex::new(())),
        coord_shard_wallets: std::sync::Arc::new(Vec::new()),
        coord_shard_round_robin: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };

    // 10) Router
    let app = build_api_router(state, &settings);

    Ok(TestCtx {
        app,
        store,
        settings,
        srv,
        node_wallet,
    })
}
