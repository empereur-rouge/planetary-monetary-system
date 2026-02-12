use std::sync::Arc;
// utils/spawn_node_rocks.rs
use anyhow::Result;
use pms_config::{ServerConfig, load_config};
use pms_core::{ConcurrentDag, CoreAdapter, ValidatePolicy};
use pms_interface::NetDagAdapter;
use pms_server::Server;
use pms_storage::DagStorage;
use tokio::task::JoinHandle;
// adapte les imports à ton projet
use pms_storage::rocks_store::store::RocksStore;
use pms_types::Block;
use pms_wallet::{SignerBackend, Wallet};

/// Démarre un nœud basé sur RocksDB et lance le serveur en tâche de fond.
/// - `db_path`: chemin de la DB Rocks (éphémère dans /tmp pour les tests)
/// - `prefix`: namespace logique (ex: "pms:test:A")
/// - `bind_addr`: ex "127.0.0.1:7401" (p2p)
/// - `api_addr`:  ex "127.0.0.1:7401" (HTTP, si nécessaire dans ton Server)
/// - `node_seed`: optional seed for unique node identity (default: [1u8; 32])
/// - `forced_genesis`: optional genesis block to inject
///
/// Retourne: (store, dag_ref, adapter, server, join_handle)
pub async fn spawn_node_generic_rocks(
    db_path: &str,
    prefix: &str,
    bind_addr: &str,
    api_addr: &str,
    tip_limit: usize,
    forced_genesis: Option<&Block>,
) -> Result<(
    Arc<RocksStore>,
    Arc<ConcurrentDag>,
    Arc<dyn NetDagAdapter>,
    Arc<Server>,
    JoinHandle<Result<()>>,
)> {
    // Delegate to the new function with default seed
    spawn_node_generic_rocks_with_seed(
        db_path,
        prefix,
        bind_addr,
        api_addr,
        tip_limit,
        forced_genesis,
        None,
        None,
        false,
    )
    .await
}

/// Same as spawn_node_generic_rocks but with custom wallet seed for unique node_id
pub async fn spawn_node_generic_rocks_with_seed(
    db_path: &str,
    prefix: &str,
    bind_addr: &str,
    api_addr: &str,
    tip_limit: usize,
    forced_genesis: Option<&Block>,
    node_seed: Option<[u8; 32]>,
    coordinator_pk_hex: Option<String>,
    enforce_parents: bool,
) -> Result<(
    Arc<RocksStore>,
    Arc<ConcurrentDag>,
    Arc<dyn NetDagAdapter>,
    Arc<Server>,
    JoinHandle<Result<()>>,
)> {
    // 0) Config (TLS off pour tests)
    let mut settings = load_config()?;
    settings.tls = None;

    if let Some(pk) = coordinator_pk_hex {
        settings.validation.coordinator_public_key = Some(pk);
    }

    let mut net_id = settings.network.network_id.clone();
    let mut proto = settings.network.protocol_version;

    // Use provided seed or default [1u8; 32]
    let seed = node_seed.unwrap_or([1u8; 32]);
    let node_wallet = Wallet::from_seed(&seed, None).expect("wallet de test ne doit pas échouer");
    let node_wallet = Arc::new(node_wallet);
    if net_id.is_empty() {
        net_id = "pms-dev".into();
    }
    if proto == 0 {
        proto = 1;
    }

    // 1) Store Rocks
    let store = Arc::new(RocksStore::new(db_path, tip_limit, prefix, None).await?);

    // 1.bis) Schéma + GENESIS si DB vide (pipeline prod-like)
    store.ensure_schema().await?;

    let ids = store.all_block_ids().await?;
    if ids.is_empty() {
        // soit forced_genesis (test) soit genesis canonique
        let g: Block = forced_genesis
            .cloned()
            .unwrap_or_else(|| Block::genesis(pms_utils::compute_block_id));

        let meta = pms_wire::WireMeta {
            network_id: net_id.clone(),
            protocol_version: proto,
        };

        store.persist_genesis(&g, &meta).await?;
    }

    // 2) DAG depuis le store (qui contient maintenant un genesis valide)
    let dag = Arc::new(ConcurrentDag::bootstrap_from_store::<RocksStore>(&*store).await?);

    // 3) Adapter + serveur
    let mut policy = ValidatePolicy::from_settings(&settings.validation);
    policy.enforce_parent_existence = enforce_parents;
    eprintln!(
        "[TEST] spawn_rocks: enforce_parents={} policy.enforce={}",
        enforce_parents, policy.enforce_parent_existence
    );

    let adapter_concrete = CoreAdapter::new_with_policy(dag.clone(), store.clone(), policy);
    let adapter: Arc<dyn NetDagAdapter> = adapter_concrete.clone();

    let server = Server::new(adapter.clone(), &net_id, proto, node_wallet, &settings.p2p, None);

    let cfg = Arc::new(ServerConfig {
        bind_addr: bind_addr.to_string(),
        api_addr: api_addr.to_string(),
        tls: None,
        network: pms_config::Network {
            mode: pms_config::NetworkMode::Dev,
            network_id: net_id,
            protocol_version: proto,
        },
        auth: pms_config::Auth {
            require_signed_submit: false,
            admin_api_token: None,
            allowed_ips: vec![], // Tests: allow all IPs
        },
    });

    let srv = server.clone();
    let st = store.clone();
    let jh = tokio::spawn(async move { srv.run(cfg, st).await });

    Ok((store, dag, adapter, server, jh))
}
