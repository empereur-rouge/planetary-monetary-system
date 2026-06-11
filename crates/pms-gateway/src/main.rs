use anyhow::Result;
use axum::routing::{get, post};
use axum::Router;
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderValue, Method};
use std::sync::Arc;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::SmartIpKeyExtractor;
use tower_governor::GovernorLayer;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing_subscriber::{fmt, EnvFilter};

mod client;
mod health_checker;
mod routes;

/// Construit le layer CORS en fonction des origines configurées.
/// Si aucune origine n'est configurée ou si "*" est présent, autorise toutes les origines.
/// ATTENTION: En production, configurez CORS_ALLOWED_ORIGINS avec les domaines autorisés.
fn build_cors_layer(allowed_origins: &[String]) -> CorsLayer {
    let is_permissive = allowed_origins.is_empty() || allowed_origins.iter().any(|o| o == "*");

    if is_permissive {
        tracing::warn!("⚠️  CORS permissif activé (toutes origines). Configurez CORS_ALLOWED_ORIGINS en production!");
        CorsLayer::permissive()
    } else {
        tracing::info!("🔒 CORS restreint aux origines: {:?}", allowed_origins);
        let origins: Vec<HeaderValue> = allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();

        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers([AUTHORIZATION, CONTENT_TYPE])
            .allow_credentials(true)
    }
}

/// Gateway-specific settings (from environment variables only)
/// This avoids requiring the full Engine config with [rocks] section.
#[derive(Clone)]
pub struct GatewaySettings {
    pub engine_url: String,
    pub listen_addr: String,
    pub rate_limit_rps: u64,
    pub burst_size: u32,
    pub max_body_bytes: usize,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    /// Origines CORS autorisées (séparées par virgule).
    /// Si vide ou "*", autorise toutes les origines (déconseillé en production).
    pub cors_allowed_origins: Vec<String>,
    /// Path to the dashboard static files directory
    pub dashboard_path: Option<String>,
}

impl GatewaySettings {
    pub fn from_env() -> Self {
        let cors_origins = std::env::var("CORS_ALLOWED_ORIGINS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Self {
            engine_url: std::env::var("UPSTREAM_URL")
                .or_else(|_| std::env::var("ENGINE_URL"))
                .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string()),
            listen_addr: std::env::var("LISTEN_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8443".to_string()),
            rate_limit_rps: std::env::var("RATE_LIMIT_RPS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10000),
            burst_size: std::env::var("BURST_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(20000),
            max_body_bytes: std::env::var("MAX_BODY_BYTES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10 * 1024 * 1024), // 10MB default
            tls_cert: std::env::var("TLS_CERT").ok(),
            tls_key: std::env::var("TLS_KEY").ok(),
            cors_allowed_origins: cors_origins,
            dashboard_path: std::env::var("DASHBOARD_PATH").ok(),
        }
    }
}

#[derive(Clone)]
pub struct GatewayState {
    pub engine_client: Arc<client::EngineClient>,
    pub settings: Arc<GatewaySettings>,
    /// Cached snapshot of infrastructure service health statuses.
    pub services_cache: health_checker::SharedServicesCache,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install rustls crypto provider (Ring)
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    dotenvy::dotenv().ok();

    let filter = EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).with_target(true).init();

    // Load settings from environment variables (NOT from config file)
    let settings = GatewaySettings::from_env();

    tracing::info!("🚪 PMS Gateway starting...");
    tracing::info!("   Engine URL: {}", settings.engine_url);
    tracing::info!("   Listen Address: {}", settings.listen_addr);
    tracing::info!(
        "   Rate Limit: {} rps, Burst: {}",
        settings.rate_limit_rps,
        settings.burst_size
    );

    let engine_client = Arc::new(client::EngineClient::new(&settings.engine_url));

    // Initialize the services health cache and spawn the background checker
    let services_cache: health_checker::SharedServicesCache =
        Arc::new(tokio::sync::RwLock::new(health_checker::ServicesSnapshot {
            services: vec![],
            checked_at: 0,
        }));
    health_checker::spawn_health_checker(services_cache.clone());

    let state = GatewayState {
        engine_client,
        settings: Arc::new(settings.clone()),
        services_cache,
    };

    // NOTE: per_second(N) in tower-governor 0.8 means "period of N seconds"
    // (NOT "N requests per second"). Use per_nanosecond for correct rps conversion.
    let period_ns = 1_000_000_000u64 / settings.rate_limit_rps.max(1);
    let governor_conf = Box::new(
        GovernorConfigBuilder::default()
            .per_nanosecond(period_ns)
            .burst_size(settings.burst_size)
            // AUDIT M-9 (v0.9.0): SmartIpKeyExtractor lit X-Forwarded-For /
            // X-Real-IP avant de retomber sur l'IP du peer TCP. Derrière
            // Caddy (qui ajoute X-Forwarded-For par défaut), PeerIpKeyExtractor
            // voyait l'IP du proxy pour TOUS les clients → rate limit global
            // partagé, contournable et DoS-able entre utilisateurs légitimes.
            // Aligné sur l'engine (routes.rs) qui utilise déjà SmartIpKeyExtractor.
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .unwrap(),
    );

    // Health routes (no rate limiting, no auth — cached data, not sensitive)
    let health_routes = Router::new()
        .route("/livez", get(|| async { "ok" }))
        .route("/healthz", get(routes::healthz))
        .route("/services/status", get(routes::services_status))
        .with_state(state.clone());

    // API routes (with rate limiting)
    //
    // Only routes that need special handling are listed explicitly:
    //   - Handlers using /internal/* API (typed request/response)
    //   - SSE stream endpoints (need streaming proxy, not buffered)
    //
    // Everything else is caught by the fallback and proxied as-is to Engine.
    // New Engine endpoints are automatically available — no gateway change needed.
    let api_routes = Router::new()
        // Special handlers — use Engine's /internal/* API with typed payloads
        .route("/v1/tips", get(routes::get_tips))
        .route("/v1/utxos/{address}", get(routes::get_utxos))
        .route("/v1/blocks/{id}", get(routes::get_block))
        .route("/submit/block", post(routes::submit_block))
        .route("/v1/config", get(routes::get_config))
        // SSE stream endpoints — require streaming proxy (not buffered)
        .route("/blocks/stream", get(routes::proxy_stream))
        .route(
            "/v1/wallet/{address}/activity/stream",
            get(routes::proxy_stream),
        )
        // Catch-all: proxy everything else to Engine (GET, POST, PUT, DELETE…)
        .fallback(routes::proxy_fallback)
        .with_state(state.clone())
        .layer(GovernorLayer::new(governor_conf))
        .layer(RequestBodyLimitLayer::new(settings.max_body_bytes))
        .layer(build_cors_layer(&settings.cors_allowed_origins));

    // Dashboard static files (if configured)
    let dashboard_service = settings.dashboard_path.as_ref().map(|path| {
        tracing::info!(
            "📊 Dashboard enabled at /dashboard/ (serving from {})",
            path
        );
        let index_file = format!("{}/index.html", path);
        ServeDir::new(path).not_found_service(ServeFile::new(index_file))
    });

    // Merge routes
    let mut app = health_routes
        .merge(api_routes)
        .layer(TraceLayer::new_for_http());

    // Add dashboard route if configured
    if let Some(dashboard) = dashboard_service {
        app = app.nest_service("/dashboard", dashboard);
    }

    // Check if TLS is enabled
    if let (Some(cert_path), Some(key_path)) = (&settings.tls_cert, &settings.tls_key) {
        use axum_server::tls_rustls::RustlsConfig;

        tracing::info!("🔒 TLS enabled: cert={}, key={}", cert_path, key_path);
        let config = RustlsConfig::from_pem_file(cert_path, key_path).await?;
        let addr: std::net::SocketAddr = settings.listen_addr.parse()?;

        tracing::info!("🚪 Gateway listening on https://{}", addr);
        axum_server::bind_rustls(addr, config)
            .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await?;
    } else {
        // Plain HTTP (for testing/development)
        let listener = tokio::net::TcpListener::bind(&settings.listen_addr).await?;
        tracing::info!("🚪 Gateway listening on http://{}", settings.listen_addr);
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await?;
    }

    Ok(())
}
