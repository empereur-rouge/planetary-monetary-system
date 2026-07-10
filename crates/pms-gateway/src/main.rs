use anyhow::Result;
use axum::routing::{get, post};
use axum::Router;
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderValue, Method};
use std::sync::Arc;
use std::time::Duration;
use tower::limit::ConcurrencyLimitLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::SmartIpKeyExtractor;
use tower_governor::GovernorLayer;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::timeout::TimeoutLayer;
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
    /// Timeout par requête (ms) appliqué au bord public. Borne les requêtes
    /// lentes (slow-body, upstream engine lent) → 408. Ne coupe PAS les flux
    /// SSE : le handler stream retourne dès l'arrivée des headers amont, le
    /// timeout ne borne donc que le time-to-first-byte, pas la durée du flux.
    pub request_timeout_ms: u64,
    /// Plafond de requêtes concurrentes in-flight au bord public. Sans lui,
    /// un client pouvait ouvrir un nombre illimité de connexions lentes et
    /// saturer le gateway (les slots ne sont tenus que le temps du round-trip
    /// proxy ; les handlers SSE relâchent leur slot dès le retour de la
    /// réponse). Mirror du plafond engine (256).
    pub max_concurrent: usize,
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
            request_timeout_ms: std::env::var("REQUEST_TIMEOUT_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(30_000), // 30s — aligné sur le timeout reqwest amont
            max_concurrent: std::env::var("MAX_CONCURRENT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(512), // 2× le plafond engine (256) pour absorber les bursts
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

/// Construit le Router complet du gateway (health + API proxifiée + dashboard
/// optionnel) avec toute la pile de couches défensives. Extrait de `main` pour
/// être testable via `oneshot` sans ouvrir de socket TLS.
///
/// Pile de couches (ordre miroir de l'engine — la DERNIÈRE `.layer()` est la
/// plus externe / exécutée en premier) :
/// 1. Governor (rate limit per-IP via `SmartIpKeyExtractor`)
/// 2. Timeout (borne les requêtes lentes → 408)
/// 3. Body limit (rejette les corps trop gros → 413)
/// 4. Concurrency limit (borne les requêtes in-flight)
/// 5. CORS
///
/// Les routes health (`/livez`, `/healthz`, `/services/status`) restent hors
/// de cette pile : ni rate limit ni auth (données cachées, non sensibles).
pub fn build_app(state: GatewayState) -> Router {
    let settings = state.settings.clone();

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

    // API routes (with the full defensive layer stack).
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
        // SSE stream endpoints — require streaming proxy (not buffered).
        // Safe under Timeout/Concurrency : the handler returns as soon as the
        // upstream response headers arrive, so neither layer bounds the live
        // stream — only the time-to-first-byte.
        .route("/blocks/stream", get(routes::proxy_stream))
        .route(
            "/v1/wallet/{address}/activity/stream",
            get(routes::proxy_stream),
        )
        // Catch-all: proxy everything else to Engine (GET, POST, PUT, DELETE…)
        .fallback(routes::proxy_fallback)
        .with_state(state.clone())
        .layer(GovernorLayer::new(governor_conf))
        .layer(TimeoutLayer::new(Duration::from_millis(
            settings.request_timeout_ms,
        )))
        .layer(RequestBodyLimitLayer::new(settings.max_body_bytes))
        .layer(ConcurrencyLimitLayer::new(settings.max_concurrent))
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

    app
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
    tracing::info!(
        "   Timeout: {} ms, Max concurrent: {}",
        settings.request_timeout_ms,
        settings.max_concurrent
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

    let app = build_app(state);

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use http::{Request, StatusCode};
    use std::net::SocketAddr;
    use tower::ServiceExt; // for `oneshot`

    /// Settings de test : rate limit large (on ne teste pas le 429 ici),
    /// timeout court pour trancher vite le cas lent.
    fn test_settings(engine_url: String, timeout_ms: u64) -> GatewaySettings {
        GatewaySettings {
            engine_url,
            listen_addr: "127.0.0.1:0".to_string(),
            rate_limit_rps: 100_000,
            burst_size: 200_000,
            max_body_bytes: 10 * 1024 * 1024,
            request_timeout_ms: timeout_ms,
            max_concurrent: 512,
            tls_cert: None,
            tls_key: None,
            cors_allowed_origins: vec![],
            dashboard_path: None,
        }
    }

    fn test_state(settings: GatewaySettings) -> GatewayState {
        let engine_client = Arc::new(client::EngineClient::new(&settings.engine_url));
        let services_cache: health_checker::SharedServicesCache =
            Arc::new(tokio::sync::RwLock::new(health_checker::ServicesSnapshot {
                services: vec![],
                checked_at: 0,
            }));
        GatewayState {
            engine_client,
            settings: Arc::new(settings),
            services_cache,
        }
    }

    /// Engine mock qui dort `delay` avant de répondre, pour éprouver le
    /// TimeoutLayer du gateway (upstream lent).
    async fn spawn_slow_engine(delay: Duration) -> String {
        let app = Router::new().fallback(move || async move {
            tokio::time::sleep(delay).await;
            "engine-ok"
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .method("GET")
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 4321))))
            .body(Body::empty())
            .unwrap()
    }

    /// Un upstream plus lent que le timeout gateway → le bord public répond
    /// 408 sans attendre les 30s du client reqwest amont : borne les requêtes
    /// lentes au lieu de tenir un slot indéfiniment.
    #[tokio::test(flavor = "multi_thread")]
    async fn slow_upstream_hits_gateway_timeout_408() {
        let engine_url = spawn_slow_engine(Duration::from_secs(5)).await;
        let app = build_app(test_state(test_settings(engine_url, 250)));

        let res = app.oneshot(get("/v1/balance")).await.unwrap();
        println!(
            "upstream 5s + timeout gateway 250ms → status {} (attendu 408)",
            res.status()
        );
        assert_eq!(
            res.status(),
            StatusCode::REQUEST_TIMEOUT,
            "le TimeoutLayer du gateway doit couper la requête lente en 408"
        );
    }

    /// Le timeout ne casse pas le chemin normal : un upstream rapide passe.
    #[tokio::test(flavor = "multi_thread")]
    async fn fast_upstream_passes_through() {
        let engine_url = spawn_slow_engine(Duration::from_millis(10)).await;
        let app = build_app(test_state(test_settings(engine_url, 5_000)));

        let res = app.oneshot(get("/v1/balance")).await.unwrap();
        let status = res.status();
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);
        println!("upstream rapide → status {status}, body {body_str:?} (attendu 200 / engine-ok)");
        assert_eq!(status, StatusCode::OK, "un upstream rapide doit passer");
        assert_eq!(
            body_str, "engine-ok",
            "le corps de l'engine doit être proxifié tel quel"
        );
    }

    /// Les routes health restent hors de la pile défensive : `/livez` répond
    /// sans dépendre de l'engine ni du timeout.
    #[tokio::test(flavor = "multi_thread")]
    async fn health_route_bypasses_stack() {
        // engine URL bidon : /livez ne doit pas le toucher
        let app = build_app(test_state(test_settings(
            "http://127.0.0.1:1".to_string(),
            250,
        )));
        let res = app.oneshot(get("/livez")).await.unwrap();
        println!("/livez → status {} (attendu 200, sans engine)", res.status());
        assert_eq!(res.status(), StatusCode::OK);
    }
}
