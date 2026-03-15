use axum::extract::{Query, State};
use pms_config::{ServerConfig, load_config};
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_server::api::AppState;
use pms_server::api_fn::history::{PageQ, get_encrypted_history, get_plain_history};
use pms_server::{Server, stats::Stats};
use pms_storage::StoredBlock;
use pms_storage::rocks_store::store::RocksStore;
use pms_types::{Block, TxOutput};
use pms_types_payload::{AAD, EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_utils::compute_block_id;
use pms_wallet::Wallet;
use pms_wire::WireMeta;
use std::sync::{Arc, atomic::AtomicBool};

#[tokio::test]
async fn history_separation_test() -> anyhow::Result<()> {
    // 1) Store
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-separation");
    let store =
        Arc::new(RocksStore::new(db_path.to_string_lossy().as_ref(), 256, "pms:test", None).await?);

    // 2) Configuration
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);
    let cfg = Arc::new(ServerConfig {
        bind_addr: "127.0.0.1:0".into(),
        api_addr: "127.0.0.1:0".into(),
        tls: None,
        api_tls_enabled: false,
        network: settings.network.clone(),
        auth: settings.auth.clone(),
    });

    // 3) Components
    let genesis = Block::genesis(compute_block_id);
    let dag = Arc::new(ConcurrentDag::new_with_genesis(genesis.clone()));
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone(), 0, None);
    let wallet = Arc::new(Wallet::generate());
    let server = Server::new(
        adapter.clone(),
        "testnet",
        1,
        wallet.clone(),
        &pms_config::P2pConfig::default(),
        None,
    );
    let ready = Arc::new(AtomicBool::new(true));
    let stats = Arc::new(Stats::new());

    let state = AppState {
        srv: server,
        _cfg: cfg,
        _ready: ready,
        stats,
        store: store.clone(),
        admin_token: None,
        node_wallet: wallet.clone(),
        settings: Arc::new(settings.clone()),
        allowed_networks: vec![], // Tests: allow all IPs
        treasury_wallets: pms_config::TreasuryWallets::empty(),
        node_registry: pms_server::node_registry::create_registry(),
        fee_pool: pms_server::fee_pool::create_fee_pool(),
        fee_pool_registry: std::sync::Arc::new(pms_server::fee_pool::FeePoolRegistry::new()),
        api_key_store: pms_server::api_keys::create_api_key_store(None).unwrap(),
        ledger_mgr: None,
        ledger_id: "main".into(),
        effective_fees: std::sync::Arc::new(
            pms_server::api_fn::tx_helpers::resolve_effective_fees(&settings.fees, None),
        ),
        activity_cache: std::sync::Arc::new(pms_server::api_fn::activity::ActivityCache::new(1_000, 30)),
        tps_tracker: std::sync::Arc::new(pms_economics::dynamic_fee::TpsTracker::new(60)),
    };

    // 4) Insert Blocks manually into Store (to bypass validation/mining for speed)

    // A) Encrypted Block
    let enc_payload = EncryptedPayload {
        scheme: "x25519+aes256gcm".into(),
        key_version: 1,
        aad: AAD { len_hint: 0 },
        commitment: "c".into(),
        ciphertext_b64: "AA==".into(),
        recipients: vec![],
        nonce_b64: "AA==".into(),
    };
    let sb_enc = StoredBlock {
        id: "ENC-1".into(),
        parents: vec![genesis.id.clone()],
        payload_json: Some(
            serde_json::to_string(&PayloadEnvelope::Encrypted(enc_payload)).unwrap(),
        ),
        nonce: 1,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };
    store.append_block_atomic(&sb_enc).await?;

    // B) Plain Block (Mint)
    let plain_payload = PlainPayload::Mint {
        outputs: vec![TxOutput {
            address: "addr1".into(),
            amount: "100".into(),
            asset_id: None,
        }],
    };
    let sb_plain = StoredBlock {
        id: "PLAIN-1".into(),
        parents: vec![genesis.id.clone()],
        payload_json: Some(serde_json::to_string(&PayloadEnvelope::Plain(plain_payload)).unwrap()),
        nonce: 2,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };
    store.append_block_atomic(&sb_plain).await?;

    // 5) Query get_encrypted_history
    let q = Query(PageQ {
        after_ts: None,
        after_id: None,
        limit: Some(10),
    });
    let resp_enc = get_encrypted_history(State(state.clone()), q)
        .await
        .unwrap();
    let items_enc = resp_enc.0.items;

    // Verify: Should contain ENC-1, but NOT PLAIN-1
    assert!(items_enc.iter().any(|b| b.id == "ENC-1"));
    assert!(!items_enc.iter().any(|b| b.id == "PLAIN-1"));
    println!(
        "Encrypted history contains {} items (expected ENC-1)",
        items_enc.len()
    );

    // 6) Query get_plain_history
    let q2 = Query(PageQ {
        after_ts: None,
        after_id: None,
        limit: Some(10),
    });
    let resp_plain = get_plain_history(State(state), q2).await.unwrap();
    let items_plain = resp_plain.0.items;

    // Verify: Should contain PLAIN-1, but NOT ENC-1
    assert!(items_plain.iter().any(|b| b.id == "PLAIN-1"));
    assert!(!items_plain.iter().any(|b| b.id == "ENC-1"));
    println!(
        "Plain history contains {} items (expected PLAIN-1)",
        items_plain.len()
    );

    Ok(())
}
