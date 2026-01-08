// pms-server/src/api
use crate::Server;
use crate::admin::{admin_compact, admin_ping};
pub use crate::api_fn::blocks::submit_block;
use crate::api_fn::dag::get_tips;
use crate::api_fn::history::{get_encrypted_history, get_plain_history, get_wallet_history};
use crate::api_fn::nft::get_nft;
use crate::api_fn::stream_blocks::stream_blocks;
use crate::api_fn::supply::get_circulating_supply;
use crate::api_fn::transaction::wallet_send_tx;
use crate::api_fn::wallet::wallet_balance;
use crate::helper::resolve_admin_token;
use crate::stats::Stats;
use crate::tls::load_tls;
use anyhow::Result;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{
    Router,
    middleware::{self, Next},
    routing::{get, post},
};
use axum_server::bind_rustls;
use axum_server::tls_rustls::RustlsConfig;
use pms_config::{ServerConfig, Settings, load_config};
use pms_storage::rocks_store::store::RocksStore;
use pms_wallet::Wallet;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::time::sleep;
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::SmartIpKeyExtractor;
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

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

/// Construit le Router HTTP complet (public + admin + debug) avec les layers de sécurité.
/// Utilisable depuis le serveur **et** depuis les tests.
pub fn build_api_router(state: AppState, settings: &Settings) -> Router {
    // Rate limiter HTTP par IP (SmartIpKeyExtractor gère X-Forwarded-For)
    let governor_conf = Box::new(
        GovernorConfigBuilder::default()
            .per_second(settings.limits.rate_limit_rps as u64)
            .burst_size(settings.limits.burst as u32)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .unwrap(),
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

    // Endpoint: /metrics (Protected)
    let metrics = Router::new()
        .route("/metrics", get(|| async { crate::metrics::render() }))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_local_or_admin,
        ));

    // Endpoint: /admin/* (Protected)
    let admin = Router::new()
        .route("/admin/ping", get(admin_ping))
        .route("/admin/compact", post(admin_compact))
        // TODO: /admin/stats
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_local_or_admin,
        ));

    // Endpoint: /submit/block (Main ingestion)
    let submit = Router::new().route("/submit/block", post(submit_block));
    // TODO: enforce signature logic inside submit_block if not already present

    let wallet = Router::new()
        .route("/wallet/tx/send", post(wallet_send_tx))
        .route("/wallet/balance", post(wallet_balance))
        .route("/wallet/history", post(get_wallet_history));

    let blocks = Router::new().route("/blocks/stream", get(stream_blocks));

    let supply = Router::new().route("/v1/supply", get(get_circulating_supply));

    let history = Router::new()
        .route("/v1/history/encrypted", get(get_encrypted_history))
        .route("/v1/history/plain", get(get_plain_history));

    let dag_routes = Router::new().route("/v1/dag/tips", post(get_tips));

    // Endpoint: /v1/nft/:token_id (Query NFT ownership)
    // NOTE: Axum 0.7+ utilise {param} au lieu de :param pour les captures de route
    let nft_routes = Router::new().route("/v1/nft/{token_id}", get(get_nft));

    let debug = Router::new().route("/debug/slow", get(debug_slow));

    // Combine all
    Router::new()
        .merge(livez)
        .merge(live)
        .merge(healthz)
        .merge(ready)
        .merge(metrics)
        .merge(admin)
        .merge(submit)
        .merge(wallet)
        .merge(blocks)
        .merge(supply)
        .merge(history)
        .merge(dag_routes)
        .merge(nft_routes)
        .merge(debug)
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
}

pub async fn serve_api(
    addr: &str,
    srv: Arc<Server>,
    cfg: Arc<ServerConfig>,
    ready: Arc<AtomicBool>,
    stats: Arc<Stats>,
    store: Arc<RocksStore>,
) -> Result<()> {
    // 🔹 Charge la config applicative complète
    let settings = load_config()?;

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
    };

    // 🔹 Construit le Router complet
    let app = build_api_router(state, &settings);

    let addr: SocketAddr = addr.parse()?;

    // TLS / HTTP
    if let Some(tls) = cfg.tls.clone() {
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

            // NOTE: bind_rustls ne supporte pas facilement inject_connect_info pour l'instant avec axum-server simple?
            // Axum-server handle l'IP via remote_addr() dans la request extension.
            // ConnectInfo extractor d'Axum standard fonctionne avec axum::serve, pas forcément axum-server ?
            // On vérifie... Axum-server implémente MakeService qui donne l'addr.
            bind_rustls(addr, tls_cfg)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await?;
            return Ok(());
        }

        // En dev -> fallback
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

    // HTTP simple
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
