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
    // v0.30.3 : champs per-API-key absents → None → défaut = limite per-IP.
    assert_eq!(limits.api_key_rate_rps, None);
    assert_eq!(limits.api_key_burst, None);
    assert_eq!(
        limits.effective_api_key_limits(),
        (20, 40),
        "per-key non spécifié doit retomber sur la limite per-IP (20/40)"
    );
}

/// La surcharge explicite du plafond per-API-key prend le pas sur la limite
/// per-IP ; l'omission retombe sur le per-IP (v0.30.3).
#[test]
fn test_api_key_limits_override_else_fallback_to_ip() {
    use pms_config::Limits;

    // Surcharge explicite
    let overridden: Limits = serde_json::from_str(
        r#"{"max_body_bytes":1,"request_timeout_ms":1,"rate_limit_rps":1000,"burst":2000,
            "api_key_rate_rps":300,"api_key_burst":600}"#,
    )
    .unwrap();
    println!(
        "override: per-IP {}/{}, per-key {:?}/{:?} → effectif {:?}",
        overridden.rate_limit_rps,
        overridden.burst,
        overridden.api_key_rate_rps,
        overridden.api_key_burst,
        overridden.effective_api_key_limits()
    );
    assert_eq!(overridden.effective_api_key_limits(), (300, 600));

    // Omission → retombe sur per-IP (ex. config bench 100000/200000 → per-key idem,
    // pas de bottleneck des benchs 10K TPS)
    let inherited: Limits = serde_json::from_str(
        r#"{"max_body_bytes":1,"request_timeout_ms":1,"rate_limit_rps":100000,"burst":200000}"#,
    )
    .unwrap();
    println!(
        "inherit: per-IP {}/{} → per-key effectif {:?}",
        inherited.rate_limit_rps,
        inherited.burst,
        inherited.effective_api_key_limits()
    );
    assert_eq!(
        inherited.effective_api_key_limits(),
        (100000, 200000),
        "per-key omis doit hériter du per-IP desserré (anti-bottleneck bench)"
    );
}

/// Configs `etc/config/*.toml` volontairement LOOSE (dev/test/bench/local, pas
/// déployés en prod) — exemptés du garde ci-dessous. Tout AUTRE fichier est
/// traité comme déployé et doit rester borné : secure-by-default, un nouveau
/// `config.<net>.toml` est couvert sauf exemption explicite ici.
const NON_DEPLOYED_CONFIGS: &[&str] = &[
    "config.dev.toml",
    "config.local.toml",
    "config.bench.toml",
    "config.docker-test.toml",
    "config.e2e-prod.toml",
    "pms-config-user.toml",
    "pms-no-tls.toml",
];

/// Garde anti re-desserrage (durcissement anti-DoS v0.30.2) : TOUTE config
/// déployée (glob `etc/config/*.toml` moins la skip-list) doit garder un
/// `rate_limit_rps` borné. À 10000 rps le token bucket per-client est quasi
/// illimité — c'est le gap DoS qu'on vient de fermer. Lit les VRAIS fichiers
/// (pas un littéral) : un futur retour à 10000, ou un nouveau config déployé
/// loose, échoue en CI au lieu de passer en silence.
#[test]
fn deployed_configs_keep_tightened_rate_limits() {
    const MAX_SANE_RPS: i64 = 2000;
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../etc/config");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir}: {e}")) {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        if NON_DEPLOYED_CONFIGS.contains(&name.as_str()) {
            println!("{name}: SKIP (non déployé)");
            continue;
        }
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("lecture {name}: {e}"));
        let val: toml::Value =
            toml::from_str(&text).unwrap_or_else(|e| panic!("parse {name}: {e}"));
        let Some(limits) = val.get("limits") else {
            println!("{name}: pas de [limits] (rien à vérifier)");
            continue;
        };
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
        assert!(
            burst <= rps * 2,
            "{name}: burst={burst} > 2× rps ({rps}) — burst trop permissif"
        );
        checked += 1;
    }
    println!("→ {checked} configs déployées vérifiées");
    assert!(
        checked >= 3,
        "attendu ≥3 configs déployées vérifiées (prod/testnet/mainnet), trouvé {checked} — glob cassé ?"
    );
}

/// Garde sécurité (revue DoS v0.30.2) : les scripts de déploiement DOIVENT
/// générer un Caddyfile qui **écrase** `X-Forwarded-For` avec l'IP réelle du
/// peer. Sans cette ligne, Caddy append à un XFF client falsifiable et le rate
/// limiter per-IP (que le gateway forwarde à l'engine) devient contournable et
/// empoisonnable. C'est le test qui aurait attrapé le gap : `client::tests`
/// prouve seulement que le forwarding a lieu — comportement dangereux SANS
/// l'écrasement Caddy.
#[test]
fn deploy_scripts_overwrite_xff() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    for script in [
        "scripts/deploy-testnet.sh",
        "scripts/deploy-mainnet.sh",
        "scripts/deploy.sh",
    ] {
        let path = format!("{root}/{script}");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("lecture {path}: {e}"));
        let has_overwrite = text.contains("header_up X-Forwarded-For {remote_host}");
        let has_reverse_proxy = text.contains("reverse_proxy https://pms-gateway:8443");
        println!(
            "{script}: reverse_proxy pms-gateway={has_reverse_proxy}, header_up XFF overwrite={has_overwrite}"
        );
        // On ne l'exige que si le script génère bien le reverse_proxy gateway.
        if has_reverse_proxy {
            assert!(
                has_overwrite,
                "{script} génère le reverse_proxy gateway SANS `header_up X-Forwarded-For {{remote_host}}` — XFF spoofable (cf. revue sécu DoS v0.30.2)"
            );
        }
    }
}
