// pms-server/src/api/serve — TLS/HTTP server bootstrap.

use super::routes::build_api_router;
use super::state::{AppState, FeePoolRefundSink};
use super::tasks::{spawn_activity_backfill_task, spawn_activity_retention_task, spawn_consolidation_task, spawn_fee_distributor_task, spawn_governance_enact_task, spawn_inflation_mint_task, spawn_metrics_sampler_task, spawn_reserve_snapshot_task, spawn_resource_guard_task};
use crate::Server;
use crate::api_keys;
use crate::helper::resolve_admin_token;
use crate::stats::Stats;
use crate::tls::load_tls;
use anyhow::Result;
use axum_server::bind_rustls;
use axum_server::tls_rustls::RustlsConfig;
use pms_config::{TreasuryWallets, ServerConfig, load_config, load_treasury_wallets};
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::AtomicBool,
};

/// Derives a wallet address from Ed25519 and X25519 public keys.
/// This mirrors the logic in `pms_wallet::Wallet::get_address()`.
fn derive_address_from_keys(
    ed25519_hex: &str,
    x25519_hex: &str,
    hrp: &str,
) -> anyhow::Result<String> {
    use bech32::{ToBase32, Variant, encode};
    use sha2::{Digest, Sha256};

    let pub_bytes = hex::decode(ed25519_hex)?;
    let hash = Sha256::digest(&pub_bytes);
    let h20 = &hash[..20];
    let xpk = hex::decode(x25519_hex)?;

    let mut payload = Vec::with_capacity(52);
    payload.extend_from_slice(h20);
    payload.extend_from_slice(&xpk);

    Ok(encode(hrp, payload.to_base32(), Variant::Bech32m)?)
}

pub async fn serve_api(
    addr: &str,
    srv: Arc<Server>,
    cfg: Arc<ServerConfig>,
    ready: Arc<AtomicBool>,
    stats: Arc<Stats>,
    store: Arc<RocksStore>,
) -> Result<()> {
    eprintln!("[API] serve_api starting on {}", addr);

    // 🔹 Charge la config applicative complète
    let settings = load_config()?;
    eprintln!("[API] config loaded OK");

    let node_wallet = srv.node_identity_wallet();

    // 🔹 Résout le token admin
    let admin_token = settings
        .auth
        .admin_api_token
        .as_deref()
        .and_then(resolve_admin_token);

    // 🔹 Parse allowed_ips into IpNetwork for fast lookup
    let allowed_networks: Vec<ipnetwork::IpNetwork> = settings
        .auth
        .allowed_ips
        .iter()
        .filter_map(|ip_str| {
            ip_str.parse().ok().or_else(|| {
                tracing::warn!("Invalid IP/CIDR in allowed_ips: {}", ip_str);
                None
            })
        })
        .collect();

    if !allowed_networks.is_empty() {
        tracing::info!(
            "Admin IP allowlist enabled: {} networks",
            allowed_networks.len()
        );
    }

    // 🔹 Load and verify treasury wallets (if configured)
    let treasury_wallets = if let Some(ref path) = settings.admin.treasury_wallets_file {
        // Get coordinator public key for verification
        let coord_pk = settings
            .validation
            .coordinator_public_key
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!("coordinator_public_key required to verify treasury wallets")
            })?;

        match load_treasury_wallets(path, coord_pk) {
            Ok(tw) => {
                tracing::info!(
                    "✅ Treasury wallets loaded: {} addresses, signature verified",
                    tw.len()
                );
                tw
            }
            Err(e) => {
                // In dev mode, warn but continue; in prod, fail
                if cfg.network.mode.is_prod() {
                    anyhow::bail!("Failed to load treasury wallets: {}", e);
                } else {
                    tracing::warn!("⚠️ Treasury wallets not loaded (dev mode): {}", e);
                    TreasuryWallets::empty()
                }
            }
        }
    } else {
        tracing::info!("Treasury wallets file not configured, using admin.wallet_addresses");
        TreasuryWallets::empty()
    };

    let ledger_mgr = srv.ledger_manager();

    // Load dynamically-created ledgers from RocksDB persistence
    // (ownership transfers, ledgers created via API in previous runs)
    if let Some(ref mgr) = ledger_mgr {
        if let Err(e) = mgr.load_persisted_ledgers(store.as_ref()).await {
            tracing::warn!("Failed to load persisted ledger defs: {e}");
        }

        // Initialize BLOCKS_PERSISTED counters for dynamic ledgers that were
        // restored above. The main.rs init loop only covers ledgers present at
        // bootstrap time — dynamic ledgers (e.g. eden) are loaded here and
        // their counters would otherwise start at 0, causing the dashboard to
        // show a misleading gap between "DAG Size" and "Blocks Persisted".
        for instance in mgr.list_all() {
            let current = crate::metrics::BLOCKS_PERSISTED
                .with_label_values(&[&instance.id])
                .get();
            if current == 0 {
                if let Ok(total) = instance.store.block_count_estimate().await {
                    if total > 0 {
                        crate::metrics::BLOCKS_PERSISTED
                            .with_label_values(&[&instance.id])
                            .inc_by(total);
                        crate::metrics::PMS_BLOCKS_TOTAL
                            .with_label_values(&[&instance.id])
                            .set(instance.dag.len() as i64);
                        tracing::info!(
                            ledger = %instance.id,
                            persisted_total = total,
                            dag_size = instance.dag.len(),
                            "Initialized metrics for dynamic ledger"
                        );
                    }
                }
            }
        }
    }

    // Per-ledger fee pool registry — main ledger pool is pre-created
    let fee_pool_registry = Arc::new(crate::fee_pool::FeePoolRegistry::new());
    let main_fee_pool = fee_pool_registry.get_or_create("main");

    // Boot-time check: flag misconfigured treasury fees so operators see it
    // immediately instead of noticing months later in an audit log review.
    // Non-fatal — the runtime still safely redirects the cut to the node
    // pool if this case ever slips through.
    if let Err(msg) = crate::fee_distribution::validate_treasury_config(
        settings.fees.treasury_fee_percent,
        &settings.fees.treasury_addresses,
        treasury_wallets.list.len(),
    ) {
        tracing::error!(target = "pms_boot", "TREASURY CONFIG ERROR: {msg}");
    }

    // Keep a reference to the main store for the contract listener.
    // Contracts are registered on the main RocksDB, so the listener
    // needs it regardless of per-ledger store swaps.
    let main_store_for_contracts: std::sync::Arc<dyn pms_storage::ContractStorage> = store.clone();

    // Grab the main EventBus BEFORE building AppState.
    // This bus is shared across all ledgers so that NftBurnProcessed
    // events from any ledger reach the single ContractListener.
    let main_event_bus = srv.adapter_arc().event_bus();

    // ═══════════════════════════════════════════════════════════════════════
    // Coordinator shard wallets (audit follow-up to v0.7.4 — UTXO
    // accumulation bottleneck on the single coordinator address).
    //
    // When `[fees].coord_shard_count > 0`, derive that many sub-wallets
    // from `node_wallet` via HKDF-SHA256 so transaction-fee outputs can
    // round-robin across them instead of all landing on one address.
    //
    // Validation in `Settings::validate()` already rejected n=1 and n>256;
    // here we just unwrap the (in-bounds) derive_set call. A failure here
    // is a single-call ~2^-128 HKDF-out-of-curve event — fail-fast at boot
    // is correct, the operator can pick a different shard_count.
    // ═══════════════════════════════════════════════════════════════════════
    let coord_shard_wallets: Vec<pms_wallet::Wallet> = if settings.fees.coord_shard_count > 0 {
        match pms_wallet::shard_derivation::derive_coord_shard_set(
            &node_wallet,
            settings.fees.coord_shard_count,
        ) {
            Ok(v) => {
                tracing::info!(
                    target = "coord_sharding",
                    count = v.len(),
                    "Derived {} coordinator shard wallets",
                    v.len()
                );
                v
            }
            Err(e) => {
                anyhow::bail!(
                    "Failed to derive coord_shard_count={} sub-wallets: {e}",
                    settings.fees.coord_shard_count
                );
            }
        }
    } else {
        Vec::new()
    };

    // Emission budget gate (plan §3.1) — recovered from the main store's
    // persisted counter. Borrow `store` before it is moved into the literal.
    let emission_gate = Arc::new(crate::emission::EmissionGate::load(&store));

    let state = AppState {
        srv,
        _cfg: cfg.clone(),
        _ready: ready.clone(),
        stats: stats.clone(),
        store,
        admin_token,
        node_wallet,
        settings: Arc::new(settings.clone()),
        allowed_networks,
        treasury_wallets,
        node_registry: crate::node_registry::create_registry(),
        fee_pool: main_fee_pool,
        fee_pool_registry,
        api_key_store: api_keys::create_api_key_store(settings.auth.api_keys_file.as_deref())
            .unwrap_or_else(|e| {
                tracing::error!("❌ Failed to load API keys: {}", e);
                api_keys::create_api_key_store(None).expect("empty store must work")
            }),
        ledger_mgr,
        ledger_id: "main".into(),
        effective_fees: Arc::new(crate::api_fn::tx_helpers::resolve_effective_fees(
            &settings.fees,
            None,
        )),
        activity_cache: Arc::new(crate::api_fn::activity::ActivityCache::new(10_000, 30)),
        tps_tracker: Arc::new(pms_economics::dynamic_fee::TpsTracker::new(60)),
        contract_event_bus: main_event_bus.clone(),
        contract_store: main_store_for_contracts.clone(),
        compliance_lock: Arc::new(tokio::sync::Mutex::new(())),
        coord_shard_wallets: Arc::new(coord_shard_wallets),
        coord_shard_round_robin: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        read_only: Arc::new(crate::read_only::ReadOnlyMode::new()),
        webhook_store: crate::api_fn::webhooks::WebhookStore::new(),
        emission_gate,
    };

    // ═══════════════════════════════════════════════════════════════════════
    // WEBHOOK DELIVERY LOOP (Phase 4) — subscribes to BlockPersisted and
    // POSTs HMAC-signed bodies to registered SaaS callbacks. Cheap when no
    // subscriptions exist (early-exit on `store.is_empty()`).
    // ═══════════════════════════════════════════════════════════════════════
    if let Some(bus) = state.srv.adapter_arc().event_bus() {
        let store = state.webhook_store.clone();
        let ledger_id = state.ledger_id.clone();
        let rx = bus.subscribe();
        tokio::spawn(async move {
            crate::api_fn::webhooks::run_delivery_loop(store, rx, ledger_id).await;
        });
    } else {
        tracing::warn!(target: "webhook", "no event bus — webhook delivery disabled");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // AUTOMATED FEE DISTRIBUTION TASK
    // ═══════════════════════════════════════════════════════════════════════
    spawn_fee_distributor_task(state.clone());

    // ═══════════════════════════════════════════════════════════════════════
    // SCHEDULED INFLATION MINT TASK
    // ═══════════════════════════════════════════════════════════════════════
    spawn_inflation_mint_task(state.clone());

    // Preuve de réserves ancrée (protocole 2.6) — no-op si [reserves] désactivé
    spawn_reserve_snapshot_task(state.clone());

    // Auto-enact de gouvernance (plan §4) — applique les propositions dont le
    // timelock est écoulé. Check read-only intégré.
    spawn_governance_enact_task(state.clone());

    // ═══════════════════════════════════════════════════════════════════════
    // ACTIVITY ITEMS BACKFILL (one-time, 30s delayed)
    // ═══════════════════════════════════════════════════════════════════════
    spawn_activity_backfill_task(state.clone());

    // ═══════════════════════════════════════════════════════════════════════
    // METRICS SAMPLER (item 4, v0.7.4) — 5s gauge polling for the persist
    // queue, fee pool, UTXO set size, and RocksDB write-stop signal.
    // ═══════════════════════════════════════════════════════════════════════
    spawn_metrics_sampler_task(state.clone());

    // ═══════════════════════════════════════════════════════════════════════
    // ACTIVITY RETENTION (audit follow-up, v0.7.4 post-launch) — daily
    // sweep that bounds the size of addr_activity / addr_type_activity /
    // activity_items CFs when [health].activity_retention_days is set.
    // No-op when the knob is None (default).
    // ═══════════════════════════════════════════════════════════════════════
    spawn_activity_retention_task(state.clone());

    // ═══════════════════════════════════════════════════════════════════════
    // AUTO UTXO CONSOLIDATION (recommendation #5, v0.7.5) — periodic
    // self-transfer at the coordinator master address when the UTXO
    // count crosses [health].auto_consolidate_min_utxos. Bounds the
    // accumulation that fee receipts produce on busy networks. No-op
    // when [health].auto_consolidate_interval_secs is None (default).
    // ═══════════════════════════════════════════════════════════════════════
    spawn_consolidation_task(state.clone());

    // ═══════════════════════════════════════════════════════════════════════
    // RESOURCE GUARD (v0.7.23) — graceful read-only degradation under
    // memory / disk / RocksDB pressure. Flips a global flag that causes
    // write-producing API routes to return 503 with `error: read_only`
    // and pauses fee distribution + inflation mint, preferring a clean
    // 503 to clients over a SIGKILL that would lose the persist queue.
    // No-op when [health].read_only_guard_enabled is false (tests/bench).
    // ═══════════════════════════════════════════════════════════════════════
    spawn_resource_guard_task(state.clone());

    // ═══════════════════════════════════════════════════════════════════════
    // TPS LOGGER (periodic JSONL file — every 10 min)
    // ═══════════════════════════════════════════════════════════════════════
    crate::tps_logger::spawn_tps_logger(state.clone(), settings.rocks.path.clone());

    // ═══════════════════════════════════════════════════════════════════════
    // CONTRACT EVALUATION LISTENER (EventBus)
    // ═══════════════════════════════════════════════════════════════════════
    if let Some(bus) = main_event_bus {
        let sink = Arc::new(FeePoolRefundSink {
            registry: state.fee_pool_registry.clone(),
        });
        pms_contracts::spawn_contract_listener(bus, main_store_for_contracts, sink);
        tracing::info!("ContractListener spawned on EventBus");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // AUTO-REGISTER CUSTOM LEDGER OWNERS IN NODE REGISTRY
    // ═══════════════════════════════════════════════════════════════════════
    // Custom ledger owners are auto-registered in the NodeRegistry so they can
    // receive node fees from their ledgers during periodic fee distribution.
    // Without this, fees would fallback to Treasury even though the owner
    // should be rewarded for creating/maintaining the ledger.
    if let Some(ref mgr) = state.ledger_mgr {
        let mut registry = state.node_registry.write().await;

        for ledger_id in mgr.list_ids() {
            if ledger_id == "main" {
                continue; // Skip main ledger (coordinator handles it)
            }

            if let Some(instance) = mgr.get(&ledger_id) {
                if let (Some(owner_pk), Some(owner_x25519)) =
                    (&instance.def.owner_pubkey, &instance.def.owner_x25519_pubkey)
                {
                    // Derive wallet address from owner's public keys
                    match derive_address_from_keys(owner_pk, owner_x25519, "8e") {
                        Ok(wallet_addr) => {
                            // Use ledger's API URL or dummy URL for owner registration
                            // (addr is still &str here, not yet parsed to SocketAddr)
                            let api_url = format!("https://{}", addr);

                            registry.register(
                                owner_pk.clone(),
                                api_url.clone(),
                                Some(wallet_addr.clone()),
                            );

                            tracing::info!(
                                "📝 Ledger '{}' owner auto-registered: {} (wallet: {})",
                                ledger_id,
                                &owner_pk[..20.min(owner_pk.len())],
                                &wallet_addr[..20.min(wallet_addr.len())]
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                "❌ Failed to derive wallet address for ledger '{}' owner: {}",
                                ledger_id,
                                e
                            );
                        }
                    }
                }
            }
        }
    }

    // 🔹 Construit le Router complet
    eprintln!("[API] building router...");
    let app = build_api_router(state, &settings);

    let addr: SocketAddr = addr.parse()?;

    // api_tls_enabled=false → skip TLS on the API even if [tls] is configured.
    // P2P TLS is independent (handled in server.rs).
    let use_api_tls = cfg.api_tls_enabled && cfg.tls.is_some();
    eprintln!(
        "[API] binding to {} (TLS={}, api_tls_enabled={})",
        addr,
        cfg.tls.is_some(),
        cfg.api_tls_enabled
    );

    if !cfg.api_tls_enabled && cfg.tls.is_some() {
        eprintln!("[API] api_tls_enabled=false → serving plain HTTP (P2P TLS unaffected)");
    }

    // TLS / HTTP
    if use_api_tls {
        let tls = cfg.tls.as_ref()
            .ok_or_else(|| anyhow::anyhow!("api_tls_enabled=true but [tls] section missing from config"))?;
        let cert_path = Path::new(&tls.cert_pem);
        let key_path = Path::new(&tls.key_pem);
        let files_exist = cert_path.exists() && key_path.exists();

        // En prod -> crash si absent
        if cfg.network.mode.is_prod() {
            if !files_exist {
                anyhow::bail!("TLS activé en prod mais fichiers manquants");
            }
            let tls_cfg = load_tls(&tls.cert_pem, &tls.key_pem)?;
            let tls_cfg = RustlsConfig::from_config(Arc::new(tls_cfg));

            bind_rustls(addr, tls_cfg)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await?;
            return Ok(());
        }

        // En dev -> fallback si fichiers absents
        if !files_exist {
            eprintln!("[API] Fallback HTTP clair (dev)");
            let listener = tokio::net::TcpListener::bind(addr).await?;
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await?;
            return Ok(());
        }

        let tls_cfg = load_tls(&tls.cert_pem, &tls.key_pem)?;
        let tls_cfg = RustlsConfig::from_config(Arc::new(tls_cfg));
        bind_rustls(addr, tls_cfg)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await?;
        return Ok(());
    }

    // HTTP simple (no TLS or api_tls_enabled=false)
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
