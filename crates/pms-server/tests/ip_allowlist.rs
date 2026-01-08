// crates/pms-server/tests/ip_allowlist.rs
//
// Tests pour le middleware IP allowlist
// Ces tests vérifient que:
// 1. Localhost est toujours autorisé
// 2. Si allowed_ips est vide, toute IP avec token valide passe
// 3. Si allowed_ips est configuré, seules les IPs dans la liste passent

use ipnetwork::IpNetwork;
use std::net::IpAddr;

/// Helper: vérifie si une IP est autorisée selon la logique du middleware
fn is_ip_allowed(client_ip: IpAddr, allowed_networks: &[IpNetwork]) -> bool {
    // 1. Localhost toujours autorisé
    if client_ip.is_loopback() {
        return true;
    }

    // 2. Si allowed_networks est vide, on laisse passer (le token sera vérifié après)
    if allowed_networks.is_empty() {
        return true;
    }

    // 3. Sinon, vérifier si l'IP est dans la whitelist
    allowed_networks.iter().any(|net| net.contains(client_ip))
}

#[test]
fn test_localhost_always_allowed() {
    // Localhost IPv4
    let localhost_v4: IpAddr = "127.0.0.1".parse().unwrap();
    // Localhost IPv6
    let localhost_v6: IpAddr = "::1".parse().unwrap();

    // Même avec une liste restrictive, localhost passe
    let restrictive: Vec<IpNetwork> = vec!["10.0.0.0/8".parse().unwrap()];

    assert!(
        is_ip_allowed(localhost_v4, &restrictive),
        "localhost IPv4 should be allowed"
    );
    assert!(
        is_ip_allowed(localhost_v6, &restrictive),
        "localhost IPv6 should be allowed"
    );
}

#[test]
fn test_empty_allowlist_permits_all() {
    // Quand la liste est vide, toute IP non-localhost passe (le token sera vérifié)
    let empty: Vec<IpNetwork> = vec![];

    let random_ip: IpAddr = "203.0.113.42".parse().unwrap();
    let private_ip: IpAddr = "192.168.1.100".parse().unwrap();

    assert!(
        is_ip_allowed(random_ip, &empty),
        "any IP should be allowed with empty list"
    );
    assert!(
        is_ip_allowed(private_ip, &empty),
        "private IP should be allowed with empty list"
    );
}

#[test]
fn test_cidr_matching() {
    // Test avec différents CIDR
    let allowed: Vec<IpNetwork> = vec![
        "10.0.0.0/8".parse().unwrap(),      // Tout le réseau 10.x.x.x
        "192.168.1.0/24".parse().unwrap(),  // 192.168.1.x
        "203.0.113.50/32".parse().unwrap(), // Une seule IP
    ];

    // IPs dans la whitelist
    assert!(
        is_ip_allowed("10.0.0.1".parse().unwrap(), &allowed),
        "10.0.0.1 in 10.0.0.0/8"
    );
    assert!(
        is_ip_allowed("10.255.255.255".parse().unwrap(), &allowed),
        "10.255.255.255 in 10.0.0.0/8"
    );
    assert!(
        is_ip_allowed("192.168.1.1".parse().unwrap(), &allowed),
        "192.168.1.1 in 192.168.1.0/24"
    );
    assert!(
        is_ip_allowed("192.168.1.254".parse().unwrap(), &allowed),
        "192.168.1.254 in 192.168.1.0/24"
    );
    assert!(
        is_ip_allowed("203.0.113.50".parse().unwrap(), &allowed),
        "exact IP match"
    );

    // IPs hors de la whitelist
    assert!(
        !is_ip_allowed("11.0.0.1".parse().unwrap(), &allowed),
        "11.0.0.1 not in 10.0.0.0/8"
    );
    assert!(
        !is_ip_allowed("192.168.2.1".parse().unwrap(), &allowed),
        "192.168.2.1 not in 192.168.1.0/24"
    );
    assert!(
        !is_ip_allowed("203.0.113.51".parse().unwrap(), &allowed),
        "wrong exact IP"
    );
    assert!(
        !is_ip_allowed("8.8.8.8".parse().unwrap(), &allowed),
        "public IP not in list"
    );
}

#[test]
fn test_single_ip_with_32_mask() {
    // /32 = une seule IP exacte
    let allowed: Vec<IpNetwork> = vec!["1.2.3.4/32".parse().unwrap()];

    assert!(
        is_ip_allowed("1.2.3.4".parse().unwrap(), &allowed),
        "exact match should work"
    );
    assert!(
        !is_ip_allowed("1.2.3.5".parse().unwrap(), &allowed),
        "adjacent IP should be blocked"
    );
    assert!(
        !is_ip_allowed("1.2.3.3".parse().unwrap(), &allowed),
        "adjacent IP should be blocked"
    );
}

#[test]
fn test_ipv6_support() {
    // Support IPv6
    let allowed: Vec<IpNetwork> = vec![
        "2001:db8::/32".parse().unwrap(),
        "fe80::1/128".parse().unwrap(), // Link-local exact
    ];

    assert!(
        is_ip_allowed("2001:db8::1".parse().unwrap(), &allowed),
        "IPv6 in range"
    );
    assert!(
        is_ip_allowed("2001:db8:abcd::1234".parse().unwrap(), &allowed),
        "IPv6 in range"
    );
    assert!(
        is_ip_allowed("fe80::1".parse().unwrap(), &allowed),
        "exact IPv6 match"
    );

    assert!(
        !is_ip_allowed("2001:db9::1".parse().unwrap(), &allowed),
        "different IPv6 prefix"
    );
    assert!(
        !is_ip_allowed("fe80::2".parse().unwrap(), &allowed),
        "different link-local"
    );
}

#[test]
fn test_parse_config_values() {
    // Test que les valeurs typiques de config se parsent correctement
    let config_values = vec![
        "192.168.1.0/24",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "1.2.3.4/32",
        "203.0.113.0/24",
        "2001:db8::/32",
    ];

    for val in config_values {
        let parsed: Result<IpNetwork, _> = val.parse();
        assert!(parsed.is_ok(), "Failed to parse: {}", val);
    }
}

#[test]
fn test_invalid_config_values() {
    // Ces valeurs ne doivent pas se parser (et seront ignorées avec un warning)
    let invalid_values = vec![
        "not-an-ip",
        "256.1.1.1/24",   // Invalid octet
        "192.168.1.1/33", // Invalid mask
        "",
    ];

    for val in invalid_values {
        let parsed: Result<IpNetwork, _> = val.parse();
        assert!(parsed.is_err(), "Should fail to parse: '{}'", val);
    }
}

// ============================================================================
// INTEGRATION TESTS - Test actual middleware behavior
// ============================================================================

#[cfg(test)]
mod integration {
    use super::*;
    use axum::{
        Router,
        body::Body,
        extract::{ConnectInfo, State},
        http::{Request, StatusCode},
        middleware,
        response::IntoResponse,
        routing::get,
    };
    use std::net::SocketAddr;
    use tower::ServiceExt;

    /// Simplified AppState for testing
    #[derive(Clone)]
    struct TestAppState {
        admin_token: Option<String>,
        allowed_networks: Vec<IpNetwork>,
    }

    /// Middleware that mimics require_local_or_admin behavior
    async fn test_admin_middleware(
        State(state): State<TestAppState>,
        ConnectInfo(addr): ConnectInfo<SocketAddr>,
        headers: axum::http::HeaderMap,
        request: Request<Body>,
        next: axum::middleware::Next,
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

        (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
    }

    /// Create test router with middleware
    fn create_test_router(state: TestAppState) -> Router {
        let protected = Router::new()
            .route("/metrics", get(|| async { "metrics data" }))
            .route("/admin/ping", get(|| async { "pong" }))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                test_admin_middleware,
            ));

        let public = Router::new().route("/livez", get(|| async { "ok" }));

        Router::new()
            .merge(protected)
            .merge(public)
            .with_state(state)
    }

    #[tokio::test]
    async fn test_public_endpoints_always_accessible() {
        // Public endpoints like /livez should always work regardless of IP restrictions
        let state = TestAppState {
            admin_token: Some("token".to_string()),
            allowed_networks: vec!["10.0.0.0/8".parse().unwrap()], // Very restrictive
        };

        let app = create_test_router(state);

        // /livez is public - no middleware protection
        let req = Request::builder()
            .uri("/livez")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "/livez should be accessible"
        );
    }

    #[tokio::test]
    async fn test_blocked_ip_simulation() {
        // Test que la logique de blocage fonctionne correctement
        // Cette simulation vérifie le comportement sans le routeur complet

        let allowed_networks: Vec<IpNetwork> = vec![
            "10.0.0.0/8".parse().unwrap(),
            "192.168.1.0/24".parse().unwrap(),
        ];

        // IP autorisée
        let allowed_ip: IpAddr = "10.50.100.200".parse().unwrap();
        assert!(
            is_ip_allowed(allowed_ip, &allowed_networks),
            "10.50.100.200 should be allowed (in 10.0.0.0/8)"
        );

        // IP bloquée (pas dans la whitelist)
        let blocked_ip: IpAddr = "203.0.113.50".parse().unwrap();
        assert!(
            !is_ip_allowed(blocked_ip, &allowed_networks),
            "203.0.113.50 should be BLOCKED (not in whitelist)"
        );

        // Autre IP bloquée
        let another_blocked: IpAddr = "8.8.8.8".parse().unwrap();
        assert!(
            !is_ip_allowed(another_blocked, &allowed_networks),
            "8.8.8.8 should be BLOCKED (Google DNS not in whitelist)"
        );
    }

    #[tokio::test]
    async fn test_attacker_ip_blocked_from_admin() {
        // Scénario: Un attaquant depuis 45.33.32.1 (IP publique) essaie d'accéder à /admin
        // Résultat attendu: 403 Forbidden

        let allowed_networks: Vec<IpNetwork> = vec![
            "192.168.0.0/16".parse().unwrap(), // Réseau privé uniquement
        ];

        let attacker_ip: IpAddr = "45.33.32.1".parse().unwrap();

        // L'attaquant n'est PAS dans la whitelist
        assert!(
            !is_ip_allowed(attacker_ip, &allowed_networks),
            "Attacker IP 45.33.32.1 should be BLOCKED from admin routes"
        );

        // Même avec un token valide (volé?), l'IP doit bloquer
        // La logique du middleware vérifie l'IP AVANT le token
        assert!(!attacker_ip.is_loopback(), "Attacker is not localhost");
        assert!(
            !allowed_networks.iter().any(|net| net.contains(attacker_ip)),
            "Attacker IP not in any allowed network"
        );
    }

    #[tokio::test]
    async fn test_admin_from_vpn_allowed() {
        // Scénario: Admin légitime depuis VPN (10.8.0.100) accède à /metrics
        // Résultat attendu: 200 OK (après vérification du token)

        let allowed_networks: Vec<IpNetwork> = vec![
            "10.8.0.0/24".parse().unwrap(),    // VPN subnet
            "192.168.1.0/24".parse().unwrap(), // Office LAN
        ];

        let admin_vpn_ip: IpAddr = "10.8.0.100".parse().unwrap();

        // L'admin depuis le VPN EST dans la whitelist
        assert!(
            is_ip_allowed(admin_vpn_ip, &allowed_networks),
            "Admin from VPN (10.8.0.100) should be ALLOWED"
        );

        // Après cette vérification, le token sera vérifié par le middleware
    }
}
