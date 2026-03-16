// pms-server/src/api
use crate::Server;
use crate::admin::{
    admin_compact, admin_get_config, admin_ping, admin_reindex_activity,
    admin_reindex_activity_items, admin_update_config,
};
use crate::api_fn::activity::{get_wallet_activity, stream_wallet_activity};
use crate::api_fn::blocks::{get_block_by_id, submit_block};
use crate::api_fn::bridge::{
    admin_bridge_disable, admin_bridge_enable, admin_bridge_transfer, bridge_status,
    list_bridge_links,
};
use crate::api_fn::compliance::{
    admin_compliance_log, admin_freeze, admin_list_frozen, admin_reverse, admin_seize,
    admin_shadow_balance, admin_unfreeze,
};
use crate::api_fn::contracts::{
    get_contract, list_contracts, register_contract, toggle_contract,
};
use crate::api_fn::coordinator::get_coordinator_info;
use crate::api_fn::gas_pool::{admin_gas_pool_deposit, admin_gas_pool_withdraw, get_gas_pool};
use crate::api_fn::version::get_version;
use crate::api_fn::dag::get_tips;
use crate::api_fn::history::{get_encrypted_history, get_plain_history, get_wallet_history};
use crate::api_fn::ledger::{
    admin_create_ledger, admin_get_ledger, admin_list_ledgers, list_ledgers,
};
use crate::api_fn::milestone::{distribute_fees, get_fee_pool_status};
use crate::api_fn::nft::{
    burn_nft, burn_nft_batch_simple, burn_nft_simple, get_nft, get_nfts_by_owner, mint_nft,
    prepare_nft_transfer,
};
use crate::api_fn::nodes::{connect_peer, list_nodes, list_peers, node_heartbeat, register_node};
use crate::api_fn::stream_blocks::stream_blocks;
use crate::api_fn::supply::get_circulating_supply;
use crate::api_fn::token::{admin_create_token, admin_mint_token, get_token, list_tokens};
use crate::api_fn::transaction::{prepare_tx, wallet_send_tx};
use crate::api_fn::wallet::{balance_by_address, wallet_balance};
use crate::api_fn::wallet_factory::{
    faucet_mint, wallet_create, wallet_restore_mnemonic, wallet_restore_private_key,
    wallet_send_simple,
};
use crate::api_keys::{self, ApiKeyCreateRequest, SharedApiKeyStore};
use crate::helper::resolve_admin_token;
use crate::stats::Stats;
use crate::tls::load_tls;
use anyhow::Result;
use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    Router,
    middleware::{self, Next},
    routing::{any, get, post},
};
use axum_server::bind_rustls;
use axum_server::tls_rustls::RustlsConfig;
use pms_config::{ServerConfig, Settings, TreasuryWallets, load_config, load_treasury_wallets};
use pms_storage::rocks_store::store::RocksStore;
use pms_wallet::Wallet;
use serde_json::json;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::time::sleep;
use tower::ServiceExt as _;
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::SmartIpKeyExtractor;
use tower_http::{
    catch_panic::CatchPanicLayer,
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    services::ServeDir,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

// ═══════════════════════════════════════════════════════════════════════════
// RefundSink implementation for pms-contracts listener
// ═══════════════════════════════════════════════════════════════════════════

/// Adapts `FeePoolRegistry` to the `RefundSink` trait required by `pms-contracts`.
///
/// Each call routes the refund to the correct per-ledger FeePool.
struct FeePoolRefundSink {
    registry: Arc<crate::fee_pool::FeePoolRegistry>,
}

impl pms_contracts::RefundSink for FeePoolRefundSink {
    fn add_burn_refund(
        &self,
        ledger_id: &str,
        address: &str,
        amount: rust_decimal::Decimal,
        asset_id: Option<String>,
    ) {
        let pool = self.registry.get_or_create(ledger_id);
        // Use try_write to avoid blocking the EventBus listener task.
        // If the lock is held (fee distribution in progress), use blocking write.
        match pool.try_write() {
            Ok(mut guard) => {
                guard.add_burn_refund(address, amount, asset_id);
            }
            Err(_) => {
                // Fallback: spawn a blocking write to avoid deadlock.
                // This is rare — only when fee distribution holds the write lock.
                let pool = pool.clone();
                let address = address.to_string();
                tokio::spawn(async move {
                    pool.write().await.add_burn_refund(&address, amount, asset_id);
                });
            }
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub srv: Arc<Server>,
    pub _cfg: Arc<ServerConfig>,
    pub _ready: Arc<AtomicBool>,
    pub stats: Arc<Stats>,
    pub store: Arc<RocksStore>,
    /// Token admin déjà résolu (valeur réelle, pas "env:XXX").
    /// None = pas d'API admin active.
    pub admin_token: Option<String>,
    pub node_wallet: Arc<Wallet>,
    /// Settings for API handlers (fees, admin addresses, etc.)
    pub settings: Arc<Settings>,
    /// Parsed IP networks for admin access (from allowed_ips config)
    /// Empty = allow all with token, non-empty = whitelist mode
    pub allowed_networks: Vec<ipnetwork::IpNetwork>,
    /// Verified treasury wallet addresses (signed by coordinator)
    pub treasury_wallets: TreasuryWallets,
    /// Dynamic node registry for distributed TX processing
    pub node_registry: crate::node_registry::SharedNodeRegistry,
    /// Fee pool for accumulating fees until Milestone distribution.
    /// In multi-ledger mode, this points to the **current ledger's** pool
    /// (set by `dynamic_ledger_handler` via `fee_pool_registry`).
    pub fee_pool: crate::fee_pool::SharedFeePool,
    /// Per-ledger fee pool registry. Each ledger gets an isolated pool so
    /// that burn refunds (e.g. EDN on eden) are distributed on the correct ledger.
    pub fee_pool_registry: Arc<crate::fee_pool::FeePoolRegistry>,
    /// Multi-ledger manager (Phase 2).
    /// Quand présent, les routes /l/{ledger_id}/* sont actives.
    pub ledger_mgr: Option<Arc<pms_ledger::LedgerManager>>,
    /// Ledger ID for this request context ("main" by default).
    pub ledger_id: String,
    /// Resolved fee configuration for this ledger context.
    pub effective_fees: Arc<crate::api_fn::tx_helpers::EffectiveFees>,
    /// Store des clés API pour l'authentification des clients SDK.
    /// Protégé par un RwLock pour lectures concurrentes (middleware)
    /// et écritures exclusives (CRUD admin).
    pub api_key_store: SharedApiKeyStore,
    /// In-memory cache for activity endpoint responses.
    pub activity_cache: Arc<crate::api_fn::activity::ActivityCache>,
    /// TPS tracker for dynamic fee calculation (congestion-based multiplier).
    pub tps_tracker: Arc<pms_economics::dynamic_fee::TpsTracker>,
    /// Main EventBus for contract evaluation.
    /// Burns on ANY ledger emit `NftBurnProcessed` to this bus,
    /// where the `ContractListener` is subscribed.
    /// `None` in test contexts where contracts are not needed.
    pub contract_event_bus: Option<pms_event::EventBus>,
}

/// Sync the PMS_BLOCKS_TOTAL gauge with the actual in-memory DAG size for the default ledger.
fn sync_dag_size_metric(st: &AppState) {
    sync_dag_size_metric_for(st, &st.ledger_id);
}

/// Sync PMS_BLOCKS_TOTAL for a specific ledger.
fn sync_dag_size_metric_for(st: &AppState, ledger_id: &str) {
    if let Some(ref mgr) = st.ledger_mgr {
        if let Some(instance) = mgr.get(ledger_id) {
            crate::metrics::PMS_BLOCKS_TOTAL
                .with_label_values(&[ledger_id])
                .set(instance.dag.len() as i64);
        }
    }
}

/// Sync PMS_BLOCKS_TOTAL for all ledgers.
fn sync_all_dag_size_metrics(st: &AppState) {
    if let Some(ref mgr) = st.ledger_mgr {
        for instance in mgr.list_all() {
            crate::metrics::PMS_BLOCKS_TOTAL
                .with_label_values(&[&instance.id])
                .set(instance.dag.len() as i64);
        }
    }
}

/// Middleware to check if request is allowed for admin routes.
/// Logic:
/// 1. Allow localhost always
/// 2. If allowed_ips is configured (non-empty), check IP is in whitelist
/// 3. Require valid admin token
async fn require_local_or_admin(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> impl IntoResponse {
    let client_ip = addr.ip();

    // 1. Always allow localhost
    if client_ip.is_loopback() {
        return next.run(request).await;
    }

    // 2. Check IP allowlist (if configured)
    if !state.allowed_networks.is_empty() {
        let ip_allowed = state
            .allowed_networks
            .iter()
            .any(|net| net.contains(client_ip));
        if !ip_allowed {
            tracing::warn!("Admin access denied: IP {} not in allowlist", client_ip);
            return (StatusCode::FORBIDDEN, "IP not allowed").into_response();
        }
    }

    // 3. Require valid Admin Token
    if let Some(token) = &state.admin_token {
        if let Some(auth_header) = headers.get("Authorization") {
            if let Ok(auth_str) = auth_header.to_str() {
                if auth_str == format!("Bearer {}", token) {
                    return next.run(request).await;
                }
            }
        }
    }

    // Block otherwise
    (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
}

/// Admin-token-only middleware for per-ledger admin routes (used inside oneshot router
/// where ConnectInfo may not be available).
async fn require_admin_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> impl IntoResponse {
    if let Some(token) = &state.admin_token {
        if let Some(auth_header) = headers.get("Authorization") {
            if let Ok(auth_str) = auth_header.to_str() {
                if auth_str == format!("Bearer {}", token) {
                    return next.run(request).await;
                }
            }
        }
    }
    (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
}

/// Middleware pour vérifier la clé API (header `X-API-Key`) sur les routes publiques.
///
/// Comportement :
/// - Si Bearer admin token valide → passe (admin bypass)
/// - Si le store est vide → passe tout (mode dev, backward-compatible)
/// - Si X-API-Key absent → 401 "Missing API Key"
/// - Si clé invalide → 403 "Invalid API Key"
/// - Si clé révoquée → 403 "API Key revoked"
/// - Si scope insuffisant → 403 "Insufficient permissions"
async fn require_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> impl IntoResponse {
    // Admin bypass : un admin token valide donne accès à toutes les routes
    if crate::helper::is_admin_authorized(&state, &headers) {
        return next.run(request).await;
    }

    // Lire le store (read lock — non-bloquant pour les autres lecteurs)
    let store = state.api_key_store.read().await;

    // Mode dev : si aucune clé n'est configurée, on laisse tout passer
    if store.is_empty() {
        drop(store); // Libérer le lock avant de continuer
        return next.run(request).await;
    }

    // Extraire le header X-API-Key
    let api_key = match headers.get("X-API-Key") {
        Some(value) => match value.to_str() {
            Ok(s) => s,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "Invalid X-API-Key header encoding"})),
                )
                    .into_response();
            }
        },
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": "Missing API Key",
                    "hint": "Add header X-API-Key: pk_live_... to your request"
                })),
            )
                .into_response();
        }
    };

    // Vérifier la clé (constant-time comparison du hash)
    let entry = match store.verify_key(api_key) {
        Some(entry) => entry.clone(),
        None => {
            tracing::warn!("🔑 Invalid API key attempt");
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "Invalid API Key"})),
            )
                .into_response();
        }
    };

    // Vérifier les permissions (scope vs path)
    let path = request.uri().path().to_string();
    if !api_keys::has_permission(&entry, &path) {
        tracing::warn!(
            "🔑 API key '{}' denied access to {} (scopes: {:?})",
            entry.id,
            path,
            entry.scopes
        );
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "Insufficient permissions",
                "scope_required": api_keys::resolve_scope(&path),
                "your_scopes": entry.scopes
            })),
        )
            .into_response();
    }

    // Libérer le lock avant de continuer
    drop(store);
    next.run(request).await
}

/// Construit les routes ledger-scoped (celles qui dépendent de l'adapter/store d'un ledger).
/// Retourne (public_routes, auth_routes) — les routes publiques n'exigent pas d'API key.
fn build_ledger_scoped_routes() -> (Router<AppState>, Router<AppState>) {
    // Endpoint: /submit/block (Main ingestion)
    let submit = Router::new().route("/submit/block", post(submit_block));

    let wallet = Router::new()
        .route("/wallet/tx/send", post(wallet_send_tx))
        .route("/wallet/balance", post(wallet_balance))
        .route("/wallet/history", post(get_wallet_history))
        .route("/v1/balance", post(balance_by_address))
        .route("/v1/tx/prepare", post(prepare_tx))
        .route("/v1/wallet/create", post(wallet_create))
        .route("/v1/wallet/restore/mnemonic", post(wallet_restore_mnemonic))
        .route(
            "/v1/wallet/restore/private-key",
            post(wallet_restore_private_key),
        )
        .route("/v1/wallet/send-simple", post(wallet_send_simple));

    let blocks = Router::new().route("/blocks/stream", get(stream_blocks));

    let supply = Router::new()
        .route("/v1/supply", get(get_circulating_supply))
        .route("/v1/fee_pool", get(get_fee_pool_status));

    let token_routes = Router::new()
        .route("/v1/tokens", get(list_tokens))
        .route("/v1/tokens/{asset_id}", get(get_token));

    let history = Router::new()
        .route("/v1/history/encrypted", get(get_encrypted_history))
        .route("/v1/history/plain", get(get_plain_history));

    let dag_routes = Router::new()
        .route("/v1/dag/tips", post(get_tips))
        .route("/v1/config", get(crate::api_fn::config::get_config))
        .route("/v1/blocks/{id}", get(get_block_by_id));

    let nft_routes = Router::new()
        .route("/v1/nft/{token_id}", get(get_nft))
        .route("/v1/wallet/{address}/nfts", get(get_nfts_by_owner))
        .route("/v1/nft/mint", post(mint_nft)) // Main ledger: API-key auth
        .route("/v1/nft/burn", post(burn_nft))
        .route("/v1/nft/burn-simple", post(burn_nft_simple))
        .route("/v1/nft/burn-batch-simple", post(burn_nft_batch_simple))
        .route("/v1/nft/transfer/prepare", post(prepare_nft_transfer))
        .route(
            "/v1/wallet/{address}/utxos",
            get(crate::api_fn::wallet::get_utxos_by_address),
        );

    let coordinator_routes = Router::new().route("/v1/coordinator/info", get(get_coordinator_info));

    let version_routes = Router::new().route("/v1/version", get(get_version));

    let activity_routes = Router::new()
        .route("/v1/wallet/{address}/activity", get(get_wallet_activity))
        .route(
            "/v1/wallet/{address}/activity/stream",
            get(stream_wallet_activity),
        );

    (
        // Public read-only routes (no API key required)
        Router::new()
            .merge(supply)
            .merge(coordinator_routes)
            .merge(version_routes)
            .merge(dag_routes)
            .merge(token_routes),
        // Authenticated routes (require API key)
        Router::new()
            .merge(submit)
            .merge(wallet)
            .merge(blocks)
            .merge(history)
            .merge(nft_routes)
            .merge(activity_routes),
    )
}

// ═══════════════════════════════════════════════════════════════════════
// Admin API Key CRUD Endpoints
// ═══════════════════════════════════════════════════════════════════════

/// POST /admin/api-keys — Crée une nouvelle clé API.
/// Retourne la clé en clair UNE SEULE FOIS.
async fn admin_create_api_key(
    State(state): State<AppState>,
    Json(req): Json<ApiKeyCreateRequest>,
) -> impl IntoResponse {
    let mut store = state.api_key_store.write().await;
    match store.create_key(req.label, req.scopes) {
        Ok(resp) => (StatusCode::CREATED, Json(json!(resp))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response(),
    }
}

/// GET /admin/api-keys — Liste toutes les clés (sans les hashes).
async fn admin_list_api_keys(State(state): State<AppState>) -> impl IntoResponse {
    let store = state.api_key_store.read().await;
    Json(json!({ "keys": store.list_keys() }))
}

/// DELETE /admin/api-keys/{id} — Révoque (soft-delete) une clé.
async fn admin_revoke_api_key(
    State(state): State<AppState>,
    axum::extract::Path(key_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let mut store = state.api_key_store.write().await;
    match store.revoke_key(&key_id) {
        Ok(()) => Json(json!({ "status": "revoked", "id": key_id })).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e}))).into_response(),
    }
}

/// Builds per-ledger admin routes (token management, faucet, NFT mint).
/// Uses admin-token-only middleware (no ConnectInfo needed inside oneshot).
///
/// **Security**: NFT minting on custom ledgers requires admin auth to prevent
/// unauthorized NFT creation that could exploit smart contracts (e.g. spoofing
/// nft_type to trigger contract refunds).
fn build_ledger_admin_routes(state: AppState) -> Router {
    Router::new()
        .route("/admin/tokens/create", post(admin_create_token))
        .route("/admin/tokens/mint", post(admin_mint_token))
        .route("/admin/faucet", post(faucet_mint))
        .route("/admin/nft/mint", post(mint_nft)) // NFT mint admin-only on custom ledgers
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_admin_token,
        ))
        .with_state(state)
}

/// Dynamic handler for per-ledger routes: `/l/{ledger_id}/{*rest}`
///
/// Resolves the ledger from LedgerManager at request time, builds a per-ledger
/// AppState, and forwards the request through `build_ledger_scoped_routes()`.
/// This allows dynamically created ledgers to be accessible immediately.
async fn dynamic_ledger_handler(
    State(state): State<AppState>,
    axum::extract::Path((ledger_id, rest)): axum::extract::Path<(String, String)>,
    req: Request,
) -> Response {
    let mgr = match &state.ledger_mgr {
        Some(m) => m,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "multi-ledger not enabled"})),
            )
                .into_response();
        }
    };
    let instance = match mgr.get(&ledger_id) {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("ledger '{}' not found", ledger_id)})),
            )
                .into_response();
        }
    };

    // Build per-ledger AppState
    let mut ledger_state = state.clone();
    ledger_state.srv = crate::Server::api_only(
        instance.adapter.clone(),
        &instance.def.network_id,
        instance.def.protocol_version,
        state.node_wallet.clone(),
        Some(state.srv.broadcast_sender()),
    );
    ledger_state.store = instance.store.clone();
    ledger_state.ledger_id = ledger_id.clone();
    ledger_state.fee_pool = state.fee_pool_registry.get_or_create(&ledger_id);
    ledger_state.effective_fees = Arc::new(crate::api_fn::tx_helpers::resolve_effective_fees(
        &state.settings.fees,
        instance.def.fees.as_ref(),
    ));

    // Build a router with ledger-scoped routes + per-ledger admin routes
    // **Security fix**: Apply require_api_key to auth routes on custom ledgers
    // (was missing — auth routes were previously unprotected on per-ledger handler)
    let (public_routes, auth_routes) = build_ledger_scoped_routes();
    let auth_routes = auth_routes.route_layer(middleware::from_fn_with_state(
        ledger_state.clone(),
        require_api_key,
    ));
    let router = public_routes
        .merge(auth_routes)
        .with_state(ledger_state.clone())
        .merge(build_ledger_admin_routes(ledger_state));

    // Reconstruct request with stripped path (remove /l/{ledger_id} prefix)
    let (mut parts, body) = req.into_parts();
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();
    let new_uri = format!("/{}{}", rest, query);
    parts.uri = new_uri
        .parse()
        .unwrap_or_else(|_| http::Uri::from_static("/"));
    let forwarded = Request::from_parts(parts, body);

    match router.oneshot(forwarded).await {
        Ok(response) => response,
        Err(infallible) => match infallible {},
    }
}

/// Construit le Router HTTP complet (public + admin + debug) avec les layers de sécurité.
/// Utilisable depuis le serveur **et** depuis les tests.
pub fn build_api_router(state: AppState, settings: &Settings) -> Router {
    tracing::info!(
        "🔒 Engine Rate Limit: {} rps, Burst: {}",
        settings.limits.rate_limit_rps,
        settings.limits.burst
    );

    // NOTE: per_second(N) in tower-governor 0.8 means "period of N seconds"
    // (NOT "N requests per second"). Use per_nanosecond for correct rps conversion.
    let period_ns = 1_000_000_000u64 / (settings.limits.rate_limit_rps as u64).max(1);
    let governor_conf = Box::new(
        GovernorConfigBuilder::default()
            .per_nanosecond(period_ns)
            .burst_size(settings.limits.burst as u32)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .expect("GovernorConfig: invalid rate_limit_rps or burst"),
    );

    // Endpoint: /livez (Check process UP)
    let livez = Router::new().route("/livez", get(|| async { "ok" }));
    // Alias legacy
    let live = Router::new().route("/live", get(|| async { "ok" }));

    // Endpoint: /healthz (Check DB + Ready)
    let healthz = {
        let r = state._ready.clone();
        Router::new().route(
            "/healthz",
            get(move || async move {
                if !r.load(Ordering::Relaxed) {
                    return (StatusCode::SERVICE_UNAVAILABLE, "starting");
                }
                // Check DB open (trivial car via Arc<RocksStore>, s'il est là c'est ouvert)
                // On pourrait check des métriques internes rocksdb si besoin
                (StatusCode::OK, "ready")
            }),
        )
    };
    // Alias legacy
    let ready = {
        let r = state._ready.clone();
        Router::new().route(
            "/ready",
            get(move || async move {
                if r.load(Ordering::Relaxed) {
                    "ready"
                } else {
                    "starting"
                }
            }),
        )
    };

    // Endpoint: /metrics (Protected, per-ledger for dashboard compatibility)
    // /metrics        → default ledger metrics (label-free, dashboard-compatible)
    // /metrics/all    → full Prometheus format with labels (ops/Grafana)
    // /l/{id}/metrics → specific ledger metrics (label-free, dashboard-compatible)
    let metrics = Router::new()
        .route(
            "/metrics",
            get(|State(st): State<AppState>| async move {
                // Sync DAG size gauge with actual in-memory count (reflects pruning)
                sync_dag_size_metric(&st);
                crate::metrics::render_for_ledger(&st.ledger_id)
            }),
        )
        .route(
            "/metrics/all",
            get(|State(st): State<AppState>| async move {
                sync_all_dag_size_metrics(&st);
                crate::metrics::render()
            }),
        )
        .route(
            "/l/{ledger_id}/metrics",
            get(
                |State(st): State<AppState>,
                 axum::extract::Path(ledger_id): axum::extract::Path<String>| async move {
                    sync_dag_size_metric_for(&st, &ledger_id);
                    crate::metrics::render_for_ledger(&ledger_id)
                },
            ),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_local_or_admin,
        ));

    // Endpoint: /admin/* (Protected)
    let admin = Router::new()
        .route("/admin/ping", get(admin_ping))
        .route("/admin/compact", post(admin_compact))
        .route("/admin/distribute_fees", post(distribute_fees))
        // Admin Config API - Hot-Swap de la RuntimeConfig
        .route("/admin/config", get(admin_get_config))
        .route("/admin/config", post(admin_update_config))
        // Admin Token API - Create and Mint custom tokens
        .route("/admin/tokens/create", post(admin_create_token))
        .route("/admin/tokens/mint", post(admin_mint_token))
        // Admin Ledger API - Create and manage ledgers
        .route("/admin/ledgers", get(admin_list_ledgers))
        .route("/admin/ledgers/create", post(admin_create_ledger))
        .route("/admin/ledgers/{ledger_id}", get(admin_get_ledger))
        // Admin Bridge API - Cross-ledger bridge management
        .route("/admin/bridge/enable", post(admin_bridge_enable))
        .route("/admin/bridge/disable", post(admin_bridge_disable))
        .route("/admin/bridge/transfer", post(admin_bridge_transfer))
        // Admin Faucet - Mint native PMS (dev/testnet)
        .route("/admin/faucet", post(faucet_mint))
        // Admin Compliance API - Freeze, Seize, Reverse, Shadow Balance
        .route("/admin/compliance/freeze", post(admin_freeze))
        .route("/admin/compliance/unfreeze", post(admin_unfreeze))
        .route("/admin/compliance/seize", post(admin_seize))
        .route("/admin/compliance/reverse", post(admin_reverse))
        .route("/admin/compliance/frozen", get(admin_list_frozen))
        .route("/admin/compliance/log", get(admin_compliance_log))
        .route(
            "/admin/compliance/shadow_balance",
            get(admin_shadow_balance),
        )
        // Admin Maintenance - Reindex activity
        .route("/admin/reindex-activity", post(admin_reindex_activity))
        .route("/admin/reindex-activity-items", post(admin_reindex_activity_items))
        // Admin API Key CRUD endpoints
        .route("/admin/api-keys", post(admin_create_api_key))
        .route("/admin/api-keys", get(admin_list_api_keys))
        .route(
            "/admin/api-keys/{key_id}",
            axum::routing::delete(admin_revoke_api_key),
        )
        // Admin Contract API - Declarative smart contracts
        .route("/admin/contracts", post(register_contract))
        .route("/admin/contracts", get(list_contracts))
        .route("/admin/contracts/{contract_id}", get(get_contract))
        .route(
            "/admin/contracts/{contract_id}/toggle",
            post(toggle_contract),
        )
        // Admin Gas Pool API - Per-ledger gas pool management
        .route("/admin/gas-pool/deposit", post(admin_gas_pool_deposit))
        .route("/admin/gas-pool/withdraw", post(admin_gas_pool_withdraw))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_local_or_admin,
        ));

    // Node registry endpoints for distributed TX processing (global, not per-ledger)
    let node_routes = Router::new()
        .route("/v1/register", post(register_node))
        .route("/v1/nodes", get(list_nodes))
        .route("/v1/peers", get(list_peers))
        .route("/v1/peers/connect", post(connect_peer))
        .route("/v1/heartbeat", post(node_heartbeat));

    // Multi-ledger endpoints (global)
    let ledger_routes = Router::new().route("/v1/ledgers", get(list_ledgers));

    // Gas pool endpoints (public, read-only)
    let gas_pool_routes = Router::new()
        .route("/v1/gas-pool/{ledger_id}", get(get_gas_pool));

    // Bridge endpoints (public, read-only)
    let bridge_routes = Router::new()
        .route("/v1/bridge/links", get(list_bridge_links))
        .route("/v1/bridge/status/{lock_block_id}", get(bridge_status));

    let debug = Router::new().route("/debug/slow", get(debug_slow));

    // Endpoint: /dashboard (Static Files)
    let dashboard = Router::new().nest_service("/dashboard", ServeDir::new("pms-dashboard/dist"));

    // Ledger-scoped routes (default ledger)
    let (public_ledger_routes, auth_ledger_routes) = build_ledger_scoped_routes();
    // Only authenticated routes require API key; public routes are open
    let auth_ledger_routes = auth_ledger_routes.route_layer(
        middleware::from_fn_with_state(state.clone(), require_api_key),
    );

    // Dynamic per-ledger routing: /l/{ledger_id}/{*rest}
    // Resolves the ledger at request time from LedgerManager, so newly created
    // ledgers become available immediately without restart.
    let per_ledger_router: Router<AppState> =
        Router::new().route("/l/{ledger_id}/{*rest}", any(dynamic_ledger_handler));

    // Internal API routes (used by gateway)
    let internal_routes = crate::internal_api::internal_routes();

    // Combine all
    Router::new()
        .merge(livez)
        .merge(live)
        .merge(healthz)
        .merge(ready)
        .merge(metrics)
        .merge(admin)
        .merge(internal_routes)
        .merge(public_ledger_routes)
        .merge(auth_ledger_routes)
        .merge(node_routes)
        .merge(ledger_routes)
        .merge(bridge_routes)
        .merge(gas_pool_routes)
        .merge(per_ledger_router)
        .merge(debug)
        .merge(dashboard)
        .with_state(state)
        // GLOBAL LAYERS (Reverse Order: Bottom executed first)
        // 5. Rate Limit
        .layer(GovernorLayer::new(governor_conf))
        // 4. Request Timeout (10-15s)
        .layer(TimeoutLayer::new(Duration::from_millis(
            settings.limits.request_timeout_ms as u64,
        )))
        // 3. Body Limit
        .layer(RequestBodyLimitLayer::new(
            settings.limits.max_body_bytes as usize,
        ))
        // 2. Concurrency Limit (256)
        .layer(tower::limit::ConcurrencyLimitLayer::new(256))
        // 1.5 CORS (allow any origin for frontend flexibility)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        // 1. Tracing (Top)
        .layer(TraceLayer::new_for_http())
        // 0. Catch panics in handlers → 500 instead of killing the server
        .layer(CatchPanicLayer::new())
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

    // Per-ledger fee pool registry — main ledger pool is pre-created
    let fee_pool_registry = Arc::new(crate::fee_pool::FeePoolRegistry::new());
    let main_fee_pool = fee_pool_registry.get_or_create("main");

    // Keep a reference to the main store for the contract listener.
    // Contracts are registered on the main RocksDB, so the listener
    // needs it regardless of per-ledger store swaps.
    let main_store_for_contracts: std::sync::Arc<dyn pms_storage::ContractStorage> = store.clone();

    // Grab the main EventBus BEFORE building AppState.
    // This bus is shared across all ledgers so that NftBurnProcessed
    // events from any ledger reach the single ContractListener.
    let main_event_bus = srv.adapter_arc().event_bus();

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
    };

    // ═══════════════════════════════════════════════════════════════════════
    // AUTOMATED FEE DISTRIBUTION TASK
    // ═══════════════════════════════════════════════════════════════════════
    spawn_fee_distributor_task(state.clone());

    // ═══════════════════════════════════════════════════════════════════════
    // SCHEDULED INFLATION MINT TASK
    // ═══════════════════════════════════════════════════════════════════════
    spawn_inflation_mint_task(state.clone());

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
        let tls = cfg.tls.as_ref().unwrap();
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

pub async fn debug_slow(State(_state): State<AppState>) -> impl IntoResponse {
    sleep(Duration::from_secs(5)).await;
    "slow-ok"
}

/// Spawns the fee distribution task if enabled in configuration.
/// Public for testing integration.
pub fn spawn_fee_distributor_task(state: AppState) {
    let settings = &state.settings;
    if settings.fees.distribution_interval_sec > 0 {
        let state_distrib = state.clone();
        let interval_sec = settings.fees.distribution_interval_sec;

        // Only run if Coordinator or Dev
        tokio::spawn(async move {
            tracing::info!(
                "⏰ Fee Distribution Service started (interval: {}s)",
                interval_sec
            );
            let mut interval = tokio::time::interval(Duration::from_secs(interval_sec));

            // consume first tick (immediate)
            interval.tick().await;

            loop {
                interval.tick().await; // Wait for next tick

                // 1. Distribute main ledger
                distribute_for_ledger(&state_distrib).await;

                // 2. Distribute each custom ledger from the registry
                if let Some(ref mgr) = state_distrib.ledger_mgr {
                    for lid in mgr.list_ids() {
                        if lid == "main" {
                            continue;
                        }

                        let pool = state_distrib.fee_pool_registry.get_or_create(&lid);
                        // Skip if pool is empty (no fees/refunds pending)
                        if !pool.read().await.has_fees() {
                            continue;
                        }

                        // Build per-ledger AppState with correct adapter/store/pool
                        if let Some(instance) = mgr.get(&lid) {
                            let mut ledger_state = state_distrib.clone();
                            ledger_state.srv = crate::Server::api_only(
                                instance.adapter.clone(),
                                &instance.def.network_id,
                                instance.def.protocol_version,
                                state_distrib.node_wallet.clone(),
                                Some(state_distrib.srv.broadcast_sender()),
                            );
                            ledger_state.store = instance.store.clone();
                            ledger_state.ledger_id = lid.clone();
                            ledger_state.fee_pool = pool;
                            ledger_state.effective_fees = Arc::new(
                                crate::api_fn::tx_helpers::resolve_effective_fees(
                                    &state_distrib.settings.fees,
                                    instance.def.fees.as_ref(),
                                ),
                            );

                            distribute_for_ledger(&ledger_state).await;
                        }
                    }
                }
            }
        });
    }
}

/// Run fee distribution for a single ledger's AppState.
async fn distribute_for_ledger(state: &AppState) {
    let pool_total = state.fee_pool.read().await.total_fees;

    match crate::fee_distribution::perform_fee_distribution(state, None).await {
        Ok(res) => {
            if res.success && res.total_distributed != "0" {
                tracing::info!(
                    "✅ [{}] Fee distribution: {} PMS to {} recipients",
                    state.ledger_id,
                    res.total_distributed,
                    res.num_recipients
                );
            } else if !res.success {
                tracing::warn!(
                    pool_total = %pool_total,
                    ledger = %state.ledger_id,
                    "⚠️ [{}] Fee distribution FAILED (success=false). \
                     Possible causes: empty tips (DAG over-pruned) or not coordinator. \
                     Fees are accumulating and NOT being distributed.",
                    state.ledger_id
                );
            }
            // Note: success=true with total=0 means no fees to distribute (normal)
        }
        Err(e) => {
            tracing::error!(
                pool_total = %pool_total,
                ledger = %state.ledger_id,
                error = %e,
                "❌ [{}] Fee distribution ERROR — fees blocked! \
                 Pool has {} PMS waiting. Error: {}",
                state.ledger_id, pool_total, e
            );
        }
    }
}

/// Spawns the scheduled inflation mint task if enabled in configuration.
pub fn spawn_inflation_mint_task(state: AppState) {
    let settings = &state.settings;
    if settings.fees.daily_inflation_enabled && settings.fees.annual_inflation_percent > 0.0 {
        let state_inflation = state.clone();
        let interval_sec = settings.fees.daily_inflation_interval_sec;

        tokio::spawn(async move {
            tracing::info!(
                "📊 Inflation Mint Service started (interval: {}s, rate: {}%/year)",
                interval_sec,
                state_inflation.settings.fees.annual_inflation_percent
            );
            let mut interval = tokio::time::interval(Duration::from_secs(interval_sec));

            // consume first tick (immediate)
            interval.tick().await;

            loop {
                interval.tick().await;

                match crate::fee_distribution::perform_daily_inflation_mint(&state_inflation).await
                {
                    Ok(res) => {
                        if res.success && res.total_distributed != "0" {
                            tracing::info!(
                                "📊 Inflation mint success: {} PMS distributed",
                                res.total_distributed
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!("❌ Inflation mint failed: {}", e);
                    }
                }
            }
        });
    }
}
