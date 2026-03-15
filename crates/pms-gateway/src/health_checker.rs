//! Background health checker for infrastructure services.
//!
//! Periodically polls configured services (Engine, Prometheus, Simulator, Caddy)
//! and caches the results. The cached snapshot is served by `GET /services/status`
//! so the dashboard can display service health indicators without hitting
//! each service directly.
//!
//! Configuration via environment variables:
//! - `SERVICES_MONITOR`: comma-separated list of `Name|url|expect_body` entries
//! - `SERVICES_CHECK_INTERVAL`: seconds between polls (default: 20)

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

/// Status of a single monitored service.
#[derive(Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    /// Display name (e.g. "Engine", "Gateway", "Prometheus")
    pub name: String,
    /// "up", "down", or "degraded"
    pub status: String,
    /// Optional detail (e.g. block count for Engine, or error message)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Health check latency in milliseconds
    pub latency_ms: u64,
}

/// Cached snapshot of all service statuses.
#[derive(Clone, Serialize, Deserialize)]
pub struct ServicesSnapshot {
    /// Status of each monitored service (Gateway is always first)
    pub services: Vec<ServiceStatus>,
    /// Unix timestamp (seconds) when this snapshot was taken
    pub checked_at: u64,
}

/// Thread-safe shared cache for the services snapshot.
pub type SharedServicesCache = Arc<RwLock<ServicesSnapshot>>;

/// A service to monitor (parsed from config).
#[derive(Clone)]
struct MonitoredService {
    name: String,
    url: String,
    /// If set, the response body must contain this substring for "up" status.
    /// Empty string means any 2xx response is sufficient.
    expect_body: String,
}

/// Parse the `SERVICES_MONITOR` env var into a list of services.
/// Format: `Name|url|expect_body,Name2|url2|expect_body2,...`
/// Falls back to a hardcoded default list for the standard docker-compose topology.
fn parse_monitored_services() -> Vec<MonitoredService> {
    if let Ok(val) = std::env::var("SERVICES_MONITOR") {
        val.split(',')
            .filter(|s| !s.trim().is_empty())
            .filter_map(|entry| {
                let parts: Vec<&str> = entry.trim().splitn(3, '|').collect();
                if parts.len() >= 2 {
                    Some(MonitoredService {
                        name: parts[0].to_string(),
                        url: parts[1].to_string(),
                        expect_body: parts.get(2).unwrap_or(&"").to_string(),
                    })
                } else {
                    tracing::warn!("Invalid SERVICES_MONITOR entry: {entry}");
                    None
                }
            })
            .collect()
    } else {
        // Default list for standard PMS docker-compose topology.
        // The gateway resolves service names via Docker DNS.
        vec![
            MonitoredService {
                name: "Engine".into(),
                url: "https://pms-engine:8080/internal/health".into(),
                expect_body: "ok".into(),
            },
            MonitoredService {
                name: "Prometheus".into(),
                url: "http://prometheus:9090/-/healthy".into(),
                expect_body: String::new(),
            },
            MonitoredService {
                name: "Simulator".into(),
                url: "http://pms-simulator:9090/".into(),
                expect_body: String::new(),
            },
            MonitoredService {
                name: "Caddy".into(),
                url: "http://caddy:80".into(),
                expect_body: String::new(),
            },
        ]
    }
}

/// Check a single service and return its status.
async fn check_service(
    client: &reqwest::Client,
    svc: &MonitoredService,
) -> ServiceStatus {
    let start = Instant::now();
    match client.get(&svc.url).send().await {
        Ok(resp) => {
            let latency = start.elapsed().as_millis() as u64;
            let status_code = resp.status();
            let body = resp.text().await.unwrap_or_default();

            let is_up = status_code.is_success()
                && (svc.expect_body.is_empty() || body.contains(&svc.expect_body));

            // Extract block_count from Engine's /internal/health response
            let detail = if svc.name == "Engine" {
                serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("block_count").map(|c| format!("{} blocks", c)))
            } else {
                None
            };

            ServiceStatus {
                name: svc.name.clone(),
                status: if is_up {
                    "up".into()
                } else {
                    "degraded".into()
                },
                detail,
                latency_ms: latency,
            }
        }
        Err(_) => ServiceStatus {
            name: svc.name.clone(),
            status: "down".into(),
            detail: None,
            latency_ms: start.elapsed().as_millis() as u64,
        },
    }
}

/// Spawn the background health checker task.
///
/// Polls all configured services every `SERVICES_CHECK_INTERVAL` seconds (default 20)
/// and writes the result into the shared cache. Each service is checked concurrently
/// with a short timeout (3s connect, 5s total) to avoid blocking.
pub fn spawn_health_checker(cache: SharedServicesCache) {
    let services = parse_monitored_services();
    let interval_secs: u64 = std::env::var("SERVICES_CHECK_INTERVAL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    tracing::info!(
        "Health checker: monitoring {} services every {}s: [{}]",
        services.len(),
        interval_secs,
        services
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Separate reqwest client with aggressive timeouts for health checks
    let checker_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .danger_accept_invalid_certs(true) // Engine uses self-signed TLS
        .build()
        .expect("health checker reqwest client");

    tokio::spawn(async move {
        loop {
            let mut statuses = Vec::with_capacity(services.len() + 1);

            // Gateway is always "up" (we're serving this response)
            statuses.push(ServiceStatus {
                name: "Gateway".into(),
                status: "up".into(),
                detail: None,
                latency_ms: 0,
            });

            // Check all services concurrently
            let mut handles = Vec::with_capacity(services.len());
            for svc in &services {
                let client = checker_client.clone();
                let svc = svc.clone();
                handles.push(tokio::spawn(
                    async move { check_service(&client, &svc).await },
                ));
            }
            for handle in handles {
                if let Ok(status) = handle.await {
                    statuses.push(status);
                }
            }

            let snapshot = ServicesSnapshot {
                services: statuses,
                checked_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            };
            *cache.write().await = snapshot;

            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
    });
}
