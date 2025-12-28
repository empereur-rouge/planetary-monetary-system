// crates/pms-core/tests/mint_policy_and_fees.rs

use std::sync::Arc;

use rust_decimal::Decimal;
use tempfile::tempdir;
use tokio::sync::Mutex;

use pms_config::{
    Address, Admin, Auth, FeePickMode, FeesSettings, Limits, Network, NetworkMode, Rocks,
    SecretSettings, Settings, ValidationSettings, load_config,
};
use pms_storage::rocks_store::store::RocksStore;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::{WireBlock, WireMeta};

use pms_core::dag::Dag;
use pms_core::{CoreAdapter, ValidatePolicy, tx_amounts_valid, validate_mint_policy};
use pms_errors::ValidationError;
use pms_interface::NetDagAdapter;
use pms_storage::{DagStorage, PutResult};
use pms_testkit::forge_signed_wire_block_for_test;
use pms_types::{PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput, Unlock};

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

    // 2) Un premier load_config pour bootstrapper le DAG (network_id, proto, etc.)
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let dag_loaded = Dag::bootstrap_from_store_or_new_dag(&*store, &meta).await?;
    let dag = Arc::new(Mutex::new(dag_loaded));

    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    // 3) Wallet ADMIN: on le met dans la config via variable d'environnement
    let admin_wallet = Wallet::from_seed(&[7u8; 32], None)
        .expect("Wallet admin from_seed ne doit pas fail en test");

    // On force la liste des admins à contenir exactement ce pubkey.
    // Selon ta config, adapte la clé d'env:
    // PMS__ADMIN__WALLET_ADDRESSES = '["<pubkey_hex>"]'
    // Nettoie d’abord les anciennes vars au cas où
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", admin_wallet.encoded_public_key());
    }

    // IMPORTANT: on recharge la config après avoir posé la VAR d'env,
    // car persist_block() va appeler load_config().
    let settings2 = load_config()?;
    let meta2 = WireMeta::from(&settings2);

    // 4) Prépare un payload Mint simple
    let outputs = vec![TxOutput {
        address: "dummy-address-for-test".to_string(),
        amount: "10".to_string(),
    }];
    let mint_payload = PayloadEnvelope::Plain(PlainPayload::Mint { outputs });

    // Parents = tips actuels (typiquement genesis)
    let mut parents = store.top_tips(2).await?;
    if parents.is_empty() {
        let all = store.all_block_ids().await?;
        if let Some(first) = all.first() {
            parents.push(first.clone());
        }
    }

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
            // On ne fige pas le message exact, on vérifie juste qu'on
            // passe bien par la policy Mint.
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
    // Policy avec un max de frais très bas pour le test
    let mut policy = ValidatePolicy::default();
    policy.max_fee_per_tx = Decimal::from_str_exact("1.0").unwrap();

    // Transaction bidon: 0 input / 0 output, juste pour tester la fee
    let tx = Transaction {
        inputs: Vec::<TxInput>::new(),
        outputs: Vec::<TxOutput>::new(),
        fee: "2.0".to_string(), // > 1.0 → doit casser
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

    // 2) Bootstrap DAG (config dev normale)
    let settings = load_config()?;
    assert!(matches!(settings.network.mode, NetworkMode::Dev));

    let meta = WireMeta::from(&settings);
    let dag_loaded = Dag::bootstrap_from_store_or_new_dag(&*store, &meta).await?;
    let dag = Arc::new(Mutex::new(dag_loaded));
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    // 3) Wallet ADMIN de test
    let admin_wallet = Wallet::from_seed(&[7u8; 32], None)
        .expect("Wallet admin from_seed ne doit pas fail en test");

    // Override de test : PMS_TEST_ADMIN_PUBKEY = pubkey hex de ce wallet
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", admin_wallet.encoded_public_key());
    }

    // 4) Prépare un payload Mint simple
    let outputs = vec![TxOutput {
        address: "dummy-address-for-test".to_string(),
        amount: "10".to_string(),
    }];
    let mint_payload = PayloadEnvelope::Plain(PlainPayload::Mint { outputs });

    // Parents = tips actuels (typiquement genesis)
    let mut parents = store.top_tips(2).await?;
    if parents.is_empty() {
        let all = store.all_block_ids().await?;
        if let Some(first) = all.first() {
            parents.push(first.clone());
        }
    }

    // 5) Bloc Mint signé par l'ADMIN → doit passer
    let wb_admin: WireBlock =
        forge_signed_wire_block_for_test(parents, &meta, &admin_wallet, 1, Some(mint_payload));

    let res_admin = adapter.persist_block(&wb_admin).await?;
    assert!(
        matches!(res_admin, PutResult::Inserted | PutResult::AlreadyExists),
        "bloc Mint signé par un admin en Dev devrait être accepté, obtenu: {res_admin:?}"
    );

    // Nettoyage de la variable d'env pour ne pas polluer les autres tests
    unsafe {
        std::env::remove_var("PMS_TEST_ADMIN_PUBKEY");
    }

    Ok(())
}

#[test]
fn mint_policy_rejects_non_admin_in_mainnet() {
    // 1) Faux settings "mainnet" minimal pour la policy
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
            signer_pubkeys: vec!["04deadbeef".into()], // seul admin autorisé
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
        },
        fees: FeesSettings {
            epsilon: "0.001".to_string(),
            scan_limit: 1000,
            mode: FeePickMode::Uniform,
            seed: None,
        },
    };

    // 2) WireBlock signé par un "non-admin"
    let wb = WireBlock {
        id: "dummy-id".into(),
        parents: vec![],
        payload_json: None,
        nonce: 0,
        network_id: settings.network.network_id.clone(),
        protocol_version: settings.network.protocol_version as u16,
        signer_pk_hex: "04cafebabe".into(), // ≠ 04deadbeef
        signature_hex: "dummy".into(),
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
