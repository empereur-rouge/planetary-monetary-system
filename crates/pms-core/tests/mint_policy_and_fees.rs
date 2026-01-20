// crates/pms-core/tests/mint_policy_and_fees.rs

use std::sync::Arc;

use rust_decimal::Decimal;
use tempfile::tempdir;

use pms_config::{
    Address, Admin, Auth, FeePickMode, FeesSettings, Limits, Network, NetworkMode, P2pConfig,
    Rocks, SecretSettings, Settings, ValidationSettings, load_config,
};
use pms_storage::rocks_store::store::RocksStore;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::{WireBlock, WireMeta};

use pms_core::{
    ConcurrentDag, CoreAdapter, ValidatePolicy, tx_amounts_valid, validate_mint_policy,
};
use pms_errors::ValidationError;
use pms_interface::NetDagAdapter;
use pms_storage::{DagStorage, PutResult};
use pms_testkit::forge_signed_wire_block_for_test;
use pms_types::{Block, PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput, Unlock};
use pms_utils::compute_block_id;

/// Ce test vérifie la chaîne complète "policy Mint + signature":
/// - un bloc Mint signé par un admin est accepté,
/// - le même bloc signé par un wallet non admin est rejeté par la policy.
#[tokio::test]
async fn mint_policy_enforced_on_admin_vs_non_admin() -> anyhow::Result<()> {
    // 1) DB éphémère
    let dir = tempdir()?;
    let db_path = dir.path().join("rocks-mint-policy");

    let store =
        Arc::new(RocksStore::new(db_path.to_string_lossy().as_ref(), 256, "pms:test:mint").await?);

    // 2) Load config and create genesis
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let genesis = Block::genesis(compute_block_id);
    let dag = Arc::new(ConcurrentDag::new_with_genesis(genesis.clone()));

    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    // 3) Wallet ADMIN
    let admin_wallet = Wallet::from_seed(&[7u8; 32], None)
        .expect("Wallet admin from_seed ne doit pas fail en test");

    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", admin_wallet.encoded_public_key());
    }

    let settings2 = load_config()?;
    let meta2 = WireMeta::from(&settings2);

    // 4) Prépare un payload Mint simple
    let outputs = vec![TxOutput {
        address: "dummy-address-for-test".to_string(),
        amount: "10".to_string(),
    }];
    let mint_payload = PayloadEnvelope::Plain(PlainPayload::Mint { outputs });

    // Parents = genesis
    let parents = vec![genesis.id.clone()];

    // 5) Bloc Mint signé par l'ADMIN → doit passer
    let wb_admin: WireBlock = forge_signed_wire_block_for_test(
        parents.clone(),
        &meta2,
        &admin_wallet,
        1,
        Some(mint_payload.clone()),
    );

    let res_admin = adapter.persist_block(&wb_admin).await?;
    assert!(
        matches!(res_admin, PutResult::Inserted | PutResult::AlreadyExists),
        "bloc Mint signé par un admin devrait être accepté, obtenu: {res_admin:?}"
    );

    // 6) Bloc Mint signé par un wallet NON ADMIN → doit être rejeté
    let non_admin_wallet = Wallet::from_seed(&[8u8; 32], None).expect("Wallet non-admin from_seed");

    let wb_non_admin: WireBlock =
        forge_signed_wire_block_for_test(parents, &meta2, &non_admin_wallet, 2, Some(mint_payload));

    let res_non_admin = adapter.persist_block(&wb_non_admin).await?;
    match res_non_admin {
        PutResult::Rejected(reason) => {
            assert!(
                reason.contains("mint") || reason.contains("UnauthorizedMint"),
                "Mint non-admin devrait être rejeté par la policy, got reason='{reason}'"
            );
        }
        other => {
            panic!("bloc Mint signé par un non-admin devrait être Rejected, obtenu: {other:?}");
        }
    }

    Ok(())
}

/// Test unitaire ciblé : une TxUtxo avec des frais > max_fee_per_tx
/// doit être rejetée par tx_amounts_valid.
#[test]
fn tx_utxo_with_fee_above_policy_is_rejected() {
    let mut policy = ValidatePolicy::default();
    policy.max_fee_per_tx = Decimal::from_str_exact("1.0").unwrap();

    let tx = Transaction {
        inputs: Vec::<TxInput>::new(),
        outputs: Vec::<TxOutput>::new(),
        fee: "2.0".to_string(),
        unlocks: Vec::<Unlock>::new(),
    };

    let res = tx_amounts_valid(&tx, &policy);

    match res {
        Err(ValidationError::FeeTooHigh { fee, max }) => {
            assert_eq!(
                Decimal::from_str_exact(&fee).unwrap(),
                Decimal::from_str_exact("2.0").unwrap()
            );
            assert_eq!(
                Decimal::from_str_exact(&max).unwrap(),
                Decimal::from_str_exact("1.0").unwrap()
            );
        }
        other => {
            panic!("Tx avec fee trop haute devrait être FeeTooHigh, obtenu: {other:?}");
        }
    }
}

/// Variante positive: frais dans la limite → OK.
#[test]
fn tx_utxo_with_fee_within_policy_is_accepted() {
    let mut policy = ValidatePolicy::default();
    policy.max_fee_per_tx = Decimal::from_str_exact("1.0").unwrap();

    let tx = Transaction {
        inputs: Vec::<TxInput>::new(),
        outputs: Vec::<TxOutput>::new(),
        fee: "0.5".to_string(),
        unlocks: Vec::<Unlock>::new(),
    };

    let res = tx_amounts_valid(&tx, &policy);
    assert!(
        res.is_ok(),
        "Fee <= max_fee_per_tx devrait passer, obtenu: {res:?}"
    );
}

#[tokio::test]
async fn dev_mode_mint_signed_by_admin_is_accepted() -> anyhow::Result<()> {
    // 1) DB éphémère
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-mint-dev-ok");

    let store = Arc::new(
        RocksStore::new(db_path.to_string_lossy().as_ref(), 256, "pms:test:mint-dev").await?,
    );

    // 2) Bootstrap DAG
    let settings = load_config()?;
    assert!(matches!(settings.network.mode, NetworkMode::Dev));

    let meta = WireMeta::from(&settings);
    let genesis = Block::genesis(compute_block_id);
    let dag = Arc::new(ConcurrentDag::new_with_genesis(genesis.clone()));
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    // 3) Wallet ADMIN de test
    let admin_wallet = Wallet::from_seed(&[7u8; 32], None)
        .expect("Wallet admin from_seed ne doit pas fail en test");

    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", admin_wallet.encoded_public_key());
    }

    // 4) Prépare un payload Mint simple
    let outputs = vec![TxOutput {
        address: "dummy-address-for-test".to_string(),
        amount: "10".to_string(),
    }];
    let mint_payload = PayloadEnvelope::Plain(PlainPayload::Mint { outputs });

    // Parents = genesis
    let parents = vec![genesis.id.clone()];

    // 5) Bloc Mint signé par l'ADMIN → doit passer
    let wb_admin: WireBlock =
        forge_signed_wire_block_for_test(parents, &meta, &admin_wallet, 1, Some(mint_payload));

    let res_admin = adapter.persist_block(&wb_admin).await?;
    assert!(
        matches!(res_admin, PutResult::Inserted | PutResult::AlreadyExists),
        "bloc Mint signé par un admin en Dev devrait être accepté, obtenu: {res_admin:?}"
    );

    // Nettoyage de la variable d'env
    unsafe {
        std::env::remove_var("PMS_TEST_ADMIN_PUBKEY");
    }

    Ok(())
}

#[test]
fn mint_policy_rejects_non_admin_in_mainnet() {
    // Faux settings "mainnet" minimal pour la policy
    let settings = Settings {
        rocks: Rocks {
            path: "/var/lib/pms/rocks".to_string(),
            prefix: "pms:main".to_string(),
            tip_limit: 100,
        },
        network: Network {
            mode: NetworkMode::Mainnet,
            network_id: "pms-main".into(),
            protocol_version: 1,
        },
        address: Address { hrp: "8e".into() },
        admin: Admin {
            wallet_addresses: vec![],
            signer_pubkeys: vec!["04deadbeef".into()],
            treasury_wallets_file: None,
        },
        client: None,
        tls: None,
        limits: Limits {
            max_body_bytes: 0,
            request_timeout_ms: 0,
            rate_limit_rps: 0,
            burst: 0,
        },
        auth: Auth {
            require_signed_submit: true,
            admin_api_token: None,
            allowed_ips: vec![],
        },
        secrets: SecretSettings {
            node_identity_key_path: "".to_string(),
            admin_wallet_file: "".to_string(),
        },
        validation: ValidationSettings {
            min_pow_leading_zero_bits: 0,
            max_payload_bytes: 0,
            min_parents_after_boot: 0,
            max_parents: 0,
            require_unique_parents: false,
            forbid_self_parent: false,
            max_inputs: 0,
            max_outputs: 0,
            max_tx_bytes: 0,
            max_fee_per_tx: 0,
            enforce_parent_existence: false,
            enforce_fee_recipient: false,
            allowed_fee_addresses: vec![],
            coordinator_public_key: None,
            coordinator_x25519_public_key: None,
            coordinator_tx_only: false,
        },
        fees: FeesSettings {
            epsilon: "0.001".to_string(),
            ratio: "0.035".to_string(),
            base_fee: "0.0".to_string(),
            mode: FeePickMode::Uniform,
            seed: None,
            platform_address: None,
            platform_address_signature: None,
            platform_fee_ratio: "0.45".to_string(),
            // Fee distribution fields
            treasury_fee_percent: 15,
            creator_fee_percent: 45,
            parents_fee_percent: 40,
            block_reward: "0.1".to_string(),
            annual_inflation_percent: 2.0,
            creator_reward_percent: 70,
            treasury_reward_percent: 20,
            burn_percent: 10,
            authority_public_keys: vec![],
            authority_keys_last_rotation: None,
        },
        p2p: P2pConfig {
            known_peers: String::new(),
            bind_addr: None,
        },
    };

    // WireBlock signé par un "non-admin"
    let wb = WireBlock {
        id: "dummy-id".into(),
        parents: vec![],
        payload_json: None,
        nonce: 0,
        network_id: settings.network.network_id.clone(),
        protocol_version: settings.network.protocol_version as u16,
        signer_pk_hex: "04cafebabe".into(),
        signature_hex: "dummy".into(),
        metadata: None,
    };

    let outputs = vec![TxOutput {
        address: "any".into(),
        amount: "1".into(),
    }];

    let res = validate_mint_policy(&outputs, &wb, &settings);
    match res {
        Err(ValidationError::UnauthorizedMint { .. }) => { /* OK */ }
        other => panic!("Mint non-admin en Mainnet devrait être UnauthorizedMint, got: {other:?}"),
    }
}
