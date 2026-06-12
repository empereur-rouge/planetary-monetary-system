//! Tests des handlers d'historique RÉELS (`get_encrypted_history` /
//! `get_plain_history` dans `api_fn/history.rs`), pas une ré-implémentation.
//!
//! Couvre :
//! 1. **Séparation** chiffré ↔ plain (un bloc d'un type n'apparaît pas dans l'autre flux).
//! 2. **Pagination** par curseur (`limit` + `next_after_id`/`next_after_ts`) à travers
//!    le vrai handler — ce que l'ancien `history_core.rs` (supprimé v0.9.3) prétendait
//!    tester via un `FakeStore` + une copie locale `get_encrypted_history_core`.

use axum::extract::{Query, State};
use pms_config::{ServerConfig, load_config};
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_server::api::AppState;
use pms_server::api_fn::history::{PageQ, get_encrypted_history, get_plain_history};
use pms_server::{Server, stats::Stats};
use pms_storage::StoredBlock;
use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use pms_types::{Block, TxOutput};
use pms_types_payload::{AAD, EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_utils::compute_block_id;
use pms_wallet::Wallet;
use pms_wire::WireMeta;
use std::sync::{Arc, atomic::AtomicBool};

/// Construit un `AppState` complet adossé à un RocksStore éphémère, et renvoie
/// aussi le store + la meta réseau pour insérer des blocs.
async fn build_test_state() -> anyhow::Result<(AppState, Arc<RocksStore>, WireMeta)> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-history");
    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256,
            "pms:test",
            None,
            &RocksMemoryConfig::default(),
        )
        .await?,
    );
    // Garde le tempdir vivant pour la durée du test (process court-vécu).
    std::mem::forget(dir);

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
        activity_cache: std::sync::Arc::new(pms_server::api_fn::activity::ActivityCache::new(
            1_000, 30,
        )),
        tps_tracker: std::sync::Arc::new(pms_economics::dynamic_fee::TpsTracker::new(60)),
        contract_event_bus: None,
        contract_store: store.clone(),
        compliance_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        coord_shard_wallets: std::sync::Arc::new(Vec::new()),
        coord_shard_round_robin: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        read_only: std::sync::Arc::new(pms_server::read_only::ReadOnlyMode::new()),
        webhook_store: pms_server::api_fn::webhooks::WebhookStore::new(),
    };
    Ok((state, store, meta))
}

/// Insère un bloc Mint (plain) avec un id donné, parenté au genesis logique.
async fn insert_plain_mint(
    store: &Arc<RocksStore>,
    meta: &WireMeta,
    id: &str,
    nonce: u64,
    addr: &str,
) -> anyhow::Result<()> {
    let payload = PlainPayload::Mint {
        outputs: vec![TxOutput {
            address: addr.into(),
            amount: "100".into(),
            asset_id: None,
        }],
    };
    let sb = StoredBlock {
        id: id.into(),
        parents: vec!["genesis".into()],
        payload_json: Some(serde_json::to_string(&PayloadEnvelope::Plain(payload)).unwrap()),
        nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };
    store.append_block_atomic(&sb).await?;
    Ok(())
}

#[tokio::test]
async fn history_separation_test() -> anyhow::Result<()> {
    let (state, store, meta) = build_test_state().await?;

    // A) Bloc chiffré
    let enc_payload = EncryptedPayload {
        scheme: "x25519+aes256gcm".into(),
        key_version: 1,
        aad: AAD {
            len_hint: 0,
            binding: None,
        },
        commitment: "c".into(),
        ciphertext_b64: "AA==".into(),
        recipients: vec![],
        nonce_b64: "AA==".into(),
    };
    let sb_enc = StoredBlock {
        id: "ENC-1".into(),
        parents: vec!["genesis".into()],
        payload_json: Some(serde_json::to_string(&PayloadEnvelope::Encrypted(enc_payload)).unwrap()),
        nonce: 1,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };
    store.append_block_atomic(&sb_enc).await?;

    // B) Bloc plain (Mint)
    insert_plain_mint(&store, &meta, "PLAIN-1", 2, "addr1").await?;

    // Flux chiffré : contient ENC-1, pas PLAIN-1.
    let resp_enc = get_encrypted_history(
        State(state.clone()),
        Query(PageQ {
            after_ts: None,
            after_id: None,
            limit: Some(10),
        }),
    )
    .await
    .unwrap();
    let items_enc = resp_enc.0.items;
    println!("encrypted history: {} items", items_enc.len());
    assert!(items_enc.iter().any(|b| b.id == "ENC-1"));
    assert!(!items_enc.iter().any(|b| b.id == "PLAIN-1"));

    // Flux plain : contient PLAIN-1, pas ENC-1.
    let resp_plain = get_plain_history(
        State(state),
        Query(PageQ {
            after_ts: None,
            after_id: None,
            limit: Some(10),
        }),
    )
    .await
    .unwrap();
    let items_plain = resp_plain.0.items;
    println!("plain history: {} items", items_plain.len());
    assert!(items_plain.iter().any(|b| b.id == "PLAIN-1"));
    assert!(!items_plain.iter().any(|b| b.id == "ENC-1"));

    Ok(())
}

/// Pagination RÉELLE à travers `get_plain_history` : 5 blocs, pages de 2, on suit
/// le curseur `next_after_ts`/`next_after_id` jusqu'à épuisement et on vérifie que
/// l'union des pages = les 5 ids, sans doublon, avec curseur final nul.
#[tokio::test]
async fn plain_history_paginates_via_cursor() -> anyhow::Result<()> {
    let (state, store, meta) = build_test_state().await?;

    // 5 blocs Mint distincts. Petite pause pour des timestamps croissants stables.
    for i in 0..5 {
        insert_plain_mint(&store, &meta, &format!("P{i}"), i as u64 + 1, &format!("addr{i}")).await?;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let mut seen: Vec<String> = Vec::new();
    let mut after_ts: Option<i64> = None;
    let mut after_id: Option<String> = None;
    let mut pages = 0;

    loop {
        pages += 1;
        assert!(pages <= 10, "pagination did not terminate (cursor loop?)");
        let resp = get_plain_history(
            State(state.clone()),
            Query(PageQ {
                after_ts,
                after_id: after_id.clone(),
                limit: Some(2),
            }),
        )
        .await
        .unwrap();
        let page = resp.0;
        let ids: Vec<String> = page.items.iter().map(|b| b.id.clone()).collect();
        println!(
            "page {pages}: ids={ids:?} next_after_id={:?}",
            page.next_after_id
        );
        assert!(page.items.len() <= 2, "page must respect limit=2");
        for id in &ids {
            seen.push(id.clone());
        }
        match page.next_after_id {
            Some(_) => {
                after_ts = page.next_after_ts;
                after_id = page.next_after_id;
            }
            None => break, // dernière page
        }
    }

    println!("paginated {pages} pages, seen={seen:?}");
    // Toutes les pages réunies → exactement les 5 blocs, sans doublon.
    let mut unique: Vec<String> = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(seen.len(), unique.len(), "no id should appear on two pages");
    let expected: std::collections::HashSet<String> =
        (0..5).map(|i| format!("P{i}")).collect();
    let got: std::collections::HashSet<String> = seen.into_iter().collect();
    assert_eq!(got, expected, "every inserted block must be paged exactly once");
    assert!(pages >= 3, "5 items at 2/page require at least 3 pages, got {pages}");
    Ok(())
}
