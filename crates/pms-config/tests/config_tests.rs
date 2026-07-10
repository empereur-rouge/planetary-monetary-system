//! Tests pour pms-config
//!
//! Couverture:
//! - NetworkMode (is_prod, is_non_prod)
//! - Désérialisation TOML
//! - Valeurs par défaut

use pms_config::NetworkMode;

#[test]
fn test_network_mode_is_prod() {
    assert!(NetworkMode::Mainnet.is_prod());
    assert!(!NetworkMode::Testnet.is_prod());
    assert!(!NetworkMode::Dev.is_prod());
}

#[test]
fn test_network_mode_is_non_prod() {
    assert!(!NetworkMode::Mainnet.is_non_prod());
    assert!(NetworkMode::Testnet.is_non_prod());
    assert!(NetworkMode::Dev.is_non_prod());
}

#[test]
fn test_network_mode_deserialize() {
    use serde_json;

    // Test lowercase deserialization
    let dev: NetworkMode = serde_json::from_str(r#""dev""#).unwrap();
    assert_eq!(dev, NetworkMode::Dev);

    let testnet: NetworkMode = serde_json::from_str(r#""testnet""#).unwrap();
    assert_eq!(testnet, NetworkMode::Testnet);

    let mainnet: NetworkMode = serde_json::from_str(r#""mainnet""#).unwrap();
    assert_eq!(mainnet, NetworkMode::Mainnet);
}

#[test]
fn test_fee_pick_mode_deserialize() {
    use pms_config::FeePickMode;
    use serde_json;

    let uniform: FeePickMode = serde_json::from_str(r#""uniform""#).unwrap();
    assert!(matches!(uniform, FeePickMode::Uniform));

    let round_robin: FeePickMode = serde_json::from_str(r#""roundrobin""#).unwrap();
    assert!(matches!(round_robin, FeePickMode::RoundRobin));
}

#[test]
fn test_rocks_default_tip_limit() {
    use pms_config::Rocks;
    use serde_json;

    // Rocks sans tip_limit doit avoir la valeur par défaut 200
    let rocks: Rocks = serde_json::from_str(r#"{"path": "/tmp/test"}"#).unwrap();
    assert_eq!(rocks.tip_limit, 200);
    assert_eq!(rocks.prefix, ""); // default empty

    // Rocks avec tip_limit explicite
    let rocks_custom: Rocks =
        serde_json::from_str(r#"{"path": "/tmp/test", "tip_limit": 500}"#).unwrap();
    assert_eq!(rocks_custom.tip_limit, 500);
}

#[test]
fn test_validation_settings_defaults() {
    use pms_config::ValidationSettings;
    use serde_json;

    // Test avec valeurs minimales (les defaults seront appliqués pour les champs optionnels)
    let json = r#"{
        "min_pow_leading_zero_bits": 4,
        "max_payload_bytes": 65536,
        "min_parents_after_boot": 2,
        "max_parents": 8,
        "require_unique_parents": true,
        "forbid_self_parent": true,
        "max_inputs": 64,
        "max_outputs": 64,
        "max_tx_bytes": 65536,
        "max_fee_per_tx": 1000,
        "enforce_parent_existence": true,
        "enforce_fee_recipient": false
    }"#;

    let settings: ValidationSettings = serde_json::from_str(json).unwrap();
    assert_eq!(settings.min_pow_leading_zero_bits, 4);
    assert!(settings.enforce_single_writer); // default true
    assert!(settings.coordinator_public_key.is_none());
}

#[test]
fn test_fees_settings_defaults() {
    use pms_config::FeesSettings;
    use serde_json;

    // Test avec valeurs minimales
    let json = r#"{
        "epsilon": "0.001",
        "mode": "uniform"
    }"#;

    let fees: FeesSettings = serde_json::from_str(json).unwrap();

    // Vérifier les valeurs par défaut
    assert_eq!(fees.ratio, "0.035");
    assert_eq!(fees.base_fee, "0.0000001");
    assert_eq!(fees.platform_fee_ratio, "0.45");
    assert_eq!(fees.treasury_fee_percent, 35);
    assert_eq!(fees.coordinator_fee_percent, 65);
    assert_eq!(fees.block_reward, "0.1");
    assert_eq!(fees.distribution_interval_sec, 600);
}

#[test]
fn test_tls_config_deserialize() {
    use pms_config::TlsConfig;
    use serde_json;

    let json = r#"{
        "cert_pem": "/path/to/cert.pem",
        "key_pem": "/path/to/key.pem",
        "ca_pem": "/path/to/ca.pem"
    }"#;

    let tls: TlsConfig = serde_json::from_str(json).unwrap();
    assert_eq!(tls.cert_pem, "/path/to/cert.pem");
    assert_eq!(tls.key_pem, "/path/to/key.pem");
    assert_eq!(tls.ca_pem, Some("/path/to/ca.pem".to_string()));
    assert!(tls.whitelist_fp256.is_empty());
}

#[test]
fn test_p2p_config_defaults() {
    use pms_config::P2pConfig;
    use serde_json;

    // Test désérialisation avec JSON vide (tous defaults)
    let p2p: P2pConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(p2p.known_peers, "");
    assert!(p2p.bind_addr.is_none());
    assert!(p2p.allowed_peer_ips.is_empty());
    assert!(!p2p.strict_whitelist);

    // P2P scaling limits — verify defaults match documented values
    println!("max_connections={}", p2p.max_connections);
    println!("per_peer_queue_cap={}", p2p.per_peer_queue_cap);
    println!("max_orphans={}", p2p.max_orphans);
    println!("max_inflight_requests={}", p2p.max_inflight_requests);
    println!("max_parent_deps={}", p2p.max_parent_deps);
    println!("max_peer_retries={}", p2p.max_peer_retries);
    assert_eq!(p2p.max_connections, 256);
    assert_eq!(p2p.per_peer_queue_cap, 2_000);
    assert_eq!(p2p.max_orphans, 2_000);
    assert_eq!(p2p.max_inflight_requests, 10_000);
    assert_eq!(p2p.max_parent_deps, 5_000);
    assert_eq!(p2p.max_peer_retries, 20);
}

#[test]
fn test_p2p_config_custom_values() {
    use pms_config::P2pConfig;

    let json = r#"{
        "known_peers": "node1:8443",
        "max_connections": 512,
        "per_peer_queue_cap": 5000,
        "max_orphans": 10000,
        "max_inflight_requests": 50000,
        "max_parent_deps": 20000,
        "max_peer_retries": 50
    }"#;

    let p2p: P2pConfig = serde_json::from_str(json).unwrap();
    println!("custom: max_connections={} per_peer_queue_cap={} max_orphans={} max_inflight_requests={} max_parent_deps={} max_peer_retries={}",
        p2p.max_connections, p2p.per_peer_queue_cap, p2p.max_orphans,
        p2p.max_inflight_requests, p2p.max_parent_deps, p2p.max_peer_retries);
    assert_eq!(p2p.known_peers, "node1:8443");
    assert_eq!(p2p.max_connections, 512);
    assert_eq!(p2p.per_peer_queue_cap, 5_000);
    assert_eq!(p2p.max_orphans, 10_000);
    assert_eq!(p2p.max_inflight_requests, 50_000);
    assert_eq!(p2p.max_parent_deps, 20_000);
    assert_eq!(p2p.max_peer_retries, 50);
}

#[test]
fn test_auth_config_defaults() {
    use pms_config::Auth;
    use serde_json;

    let auth: Auth = serde_json::from_str("{}").unwrap();
    assert!(!auth.require_signed_submit);
    assert!(auth.admin_api_token.is_none());
    assert!(auth.allowed_ips.is_empty());
}

#[test]
fn test_limits_deserialize() {
    use pms_config::Limits;
    use serde_json;

    let json = r#"{
        "max_body_bytes": 262144,
        "request_timeout_ms": 4000,
        "rate_limit_rps": 20,
        "burst": 40
    }"#;

    let limits: Limits = serde_json::from_str(json).unwrap();
    assert_eq!(limits.max_body_bytes, 262144);
    assert_eq!(limits.request_timeout_ms, 4000);
    assert_eq!(limits.rate_limit_rps, 20);
    assert_eq!(limits.burst, 40);
}

/// Garde anti re-desserrage (durcissement anti-DoS v0.30.2) : les configs
/// DÉPLOYÉES (prod/testnet/mainnet) doivent garder un `rate_limit_rps` borné.
/// À 10000 rps le token bucket per-client est quasi illimité — c'est le gap DoS
/// qu'on vient de fermer. Ce test lit les VRAIS fichiers TOML (pas un littéral)
/// pour qu'un futur retour à 10000 échoue en CI au lieu de passer en silence.
#[test]
fn deployed_configs_keep_tightened_rate_limits() {
    const MAX_SANE_RPS: i64 = 2000;
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    for name in [
        "config.prod.toml",
        "config.testnet.toml",
        "config.mainnet.toml",
    ] {
        let path = format!("{root}/etc/config/{name}");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("lecture {path}: {e}"));
        let val: toml::Value =
            toml::from_str(&text).unwrap_or_else(|e| panic!("parse {name}: {e}"));
        let limits = val
            .get("limits")
            .unwrap_or_else(|| panic!("{name}: table [limits] manquante"));
        let rps = limits
            .get("rate_limit_rps")
            .and_then(|v| v.as_integer())
            .unwrap_or_else(|| panic!("{name}: limits.rate_limit_rps manquant/non-entier"));
        let burst = limits
            .get("burst")
            .and_then(|v| v.as_integer())
            .unwrap_or_else(|| panic!("{name}: limits.burst manquant/non-entier"));
        println!("{name}: rate_limit_rps={rps}, burst={burst} (plafond sain ≤ {MAX_SANE_RPS})");
        assert!(
            rps <= MAX_SANE_RPS,
            "{name}: rate_limit_rps={rps} > {MAX_SANE_RPS} — re-desserrage DoS ? (cf. v0.30.2)"
        );
        // Le burst reste proportionné (≤ 2× le rps soutenu, comme prod).
        assert!(
            burst <= rps * 2,
            "{name}: burst={burst} > 2× rps ({rps}) — burst trop permissif"
        );
    }
}
