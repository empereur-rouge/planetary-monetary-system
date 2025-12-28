use axum::Router;
use pms_config::{ServerConfig, load_config};
use pms_core::Dag;
use pms_interface::NetDagAdapter;
use pms_server::api::{AppState, build_api_router};
use pms_server::stats::Stats;
use pms_server::{Server, resolve_admin_token};
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_types::Block;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::Mutex;

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
    let dag_loaded = Dag::bootstrap_from_store(&*store).await?;
    let dag = Arc::new(Mutex::new(dag_loaded));

    // 4) Adapter
    let adapter: Arc<dyn NetDagAdapter> = pms_core::CoreAdapter::new(dag.clone(), store.clone());

    // 5) Wallet de node pour les tests (en mémoire, pas de fichier)
    let node_wallet =
        Wallet::from_seed(&[1u8; 32], None).expect("wallet de test ne doit pas échouer");
    let node_wallet = Arc::new(node_wallet);

    // 6) Serveur
    let srv = Server::new(
        adapter,
        &settings.network.network_id,
        settings.network.protocol_version,
        node_wallet.clone(), // 👈 on injecte le wallet ici
    );

    // 7) ServerConfig minimal (ports pas utilisés ici)
    let cfg = Arc::new(ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        api_addr: "127.0.0.1:0".into(),
        tls: settings.tls.clone(),
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
        store,
        admin_token,
        node_wallet,
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
    let dag_loaded = Dag::bootstrap_from_store(&*store).await?;
    let dag = Arc::new(Mutex::new(dag_loaded));

    // 4) Adapter
    let adapter: Arc<dyn NetDagAdapter> = pms_core::CoreAdapter::new(dag.clone(), store.clone());

    // 5) Wallet node (en mémoire)
    let node_wallet = Arc::new(Wallet::from_seed(&[7u8; 32], None).unwrap());
    // 6) Serveur
    let srv = Server::new(
        adapter,
        &settings.network.network_id,
        settings.network.protocol_version,
        node_wallet.clone(), // ✅ injecté
    );

    // 7) ServerConfig minimal
    let cfg = Arc::new(ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        api_addr: "127.0.0.1:0".into(),
        tls: settings.tls.clone(),
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
