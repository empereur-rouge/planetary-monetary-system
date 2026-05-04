//! Tests de validation des blocs ContractRegister et ContractUpdate.
//!
//! Vérifie que seul le Coordinator peut enregistrer/modifier des contrats,
//! et que les champs obligatoires sont validés.

use pms_core::{Dag, ValidatePolicy, validate_block};
use pms_types::{Block, BlockId, PayloadEnvelope, PlainPayload};
use pms_types_contract::*;
use pms_utils::compute_block_id;
use rust_decimal::Decimal;

const COORDINATOR_PK: &str =
    "036ed4d5ad1c927fe972ef9728ac1888d237af57a488b6cbe50228fac442b5ae6b";
const RANDOM_PK: &str =
    "02aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn make_policy(coord_pk: Option<&str>) -> ValidatePolicy {
    ValidatePolicy {
        min_pow_leading_zero_bits: 0,
        max_payload_bytes: 1_000_000,
        min_parents_after_boot: 1,
        max_parents: 8,
        require_unique_parents: true,
        forbid_self_parent: true,
        max_inputs: 128,
        max_outputs: 128,
        max_tx_bytes: 100_000,
        max_fee_per_tx: Decimal::from(1_000_000),
        enforce_parent_existence: false,
        enforce_fee_recipient: false,
        allowed_fee_addresses: vec![],
        skip_utxo_checks: true,
        platform_address: None,
        platform_fee_ratio: Decimal::ZERO,
        coordinator_public_key: coord_pk.map(String::from),
        enforce_single_writer: false,
        network_id: String::new(),
    }
}

fn make_block(parents: Vec<BlockId>, payload: PlainPayload, signer_pk: Option<&str>) -> Block {
    let envelope = Some(PayloadEnvelope::Plain(payload));
    let nonce = 1;
    let id = compute_block_id(&parents, &envelope, nonce);
    Block {
        id,
        parents,
        payload: envelope,
        nonce,
        metadata: None,
        signer_pk: signer_pk.map(String::from),
        signature: None,
    }
}

fn make_valid_contract() -> Contract {
    Contract {
        contract_id: "test-contract-001".into(),
        name: "cube-burn-to-pms".into(),
        scope: ContractScope::Global,
        trigger: ContractTrigger::OnNftBurn {
            nft_type: Some("cube".into()),
        },
        actions: vec![ContractAction::AccumulateRefund {
            asset_id: None,
            formula: MintFormula::FixedRate {
                rate_numerator: 1,
                rate_denominator: 10,
            },
        }],
        enabled: true,
        version: 1,
    }
}

fn make_dag() -> Dag {
    let genesis = Block {
        id: "genesis".into(),
        parents: vec![],
        payload: None,
        nonce: 0,
        metadata: None,
        signer_pk: None,
        signature: None,
    };
    Dag::new_with_genesis(genesis)
}

// ═══════════════════════════════════════════════════════════════════════════
// ContractRegister — Coordinator Signature Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_contract_register_accepted_with_coordinator_signature() {
    let dag = make_dag();
    let policy = make_policy(Some(COORDINATOR_PK));
    let contract = make_valid_contract();

    let block = make_block(
        vec!["genesis".into()],
        PlainPayload::ContractRegister(contract),
        Some(COORDINATOR_PK),
    );

    let result = validate_block(&dag, &block, &policy);
    println!("ContractRegister with coordinator sig: {:?}", result);
    assert!(result.is_ok());
    println!("ContractRegister accepted with coordinator signature: OK");
}

#[test]
fn test_contract_register_rejected_without_coordinator_signature() {
    let dag = make_dag();
    let policy = make_policy(Some(COORDINATOR_PK));
    let contract = make_valid_contract();

    // Signed by a random key (not coordinator)
    let block = make_block(
        vec!["genesis".into()],
        PlainPayload::ContractRegister(contract),
        Some(RANDOM_PK),
    );

    let result = validate_block(&dag, &block, &policy);
    println!("ContractRegister with wrong sig: {:?}", result);
    assert!(result.is_err());
    let err_msg = format!("{:?}", result.unwrap_err());
    println!("Error: {}", err_msg);
    assert!(
        err_msg.contains("Coordinator") || err_msg.contains("coordinator"),
        "Error should mention coordinator"
    );
    println!("ContractRegister rejected without coordinator sig: OK");
}

#[test]
fn test_contract_register_rejected_with_no_signer() {
    let dag = make_dag();
    let policy = make_policy(Some(COORDINATOR_PK));
    let contract = make_valid_contract();

    // No signer at all
    let block = make_block(
        vec!["genesis".into()],
        PlainPayload::ContractRegister(contract),
        None,
    );

    let result = validate_block(&dag, &block, &policy);
    println!("ContractRegister with no signer: {:?}", result);
    assert!(result.is_err());
    println!("ContractRegister rejected with no signer: OK");
}

// ═══════════════════════════════════════════════════════════════════════════
// ContractRegister — Field Validation Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_contract_register_empty_contract_id_rejected() {
    let dag = make_dag();
    let policy = make_policy(Some(COORDINATOR_PK));
    let mut contract = make_valid_contract();
    contract.contract_id = "".into(); // Empty!

    let block = make_block(
        vec!["genesis".into()],
        PlainPayload::ContractRegister(contract),
        Some(COORDINATOR_PK),
    );

    let result = validate_block(&dag, &block, &policy);
    println!("ContractRegister empty contract_id: {:?}", result);
    assert!(result.is_err());
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(err_msg.contains("contract_id"), "Error should mention contract_id");
    println!("Empty contract_id rejected: OK");
}

#[test]
fn test_contract_register_empty_name_rejected() {
    let dag = make_dag();
    let policy = make_policy(Some(COORDINATOR_PK));
    let mut contract = make_valid_contract();
    contract.name = "  ".into(); // Whitespace only!

    let block = make_block(
        vec!["genesis".into()],
        PlainPayload::ContractRegister(contract),
        Some(COORDINATOR_PK),
    );

    let result = validate_block(&dag, &block, &policy);
    println!("ContractRegister whitespace name: {:?}", result);
    assert!(result.is_err());
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(err_msg.contains("name"), "Error should mention name");
    println!("Whitespace-only name rejected: OK");
}

#[test]
fn test_contract_register_no_actions_rejected() {
    let dag = make_dag();
    let policy = make_policy(Some(COORDINATOR_PK));
    let mut contract = make_valid_contract();
    contract.actions = vec![]; // No actions!

    let block = make_block(
        vec!["genesis".into()],
        PlainPayload::ContractRegister(contract),
        Some(COORDINATOR_PK),
    );

    let result = validate_block(&dag, &block, &policy);
    println!("ContractRegister no actions: {:?}", result);
    assert!(result.is_err());
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(
        err_msg.contains("action"),
        "Error should mention actions required"
    );
    println!("No actions rejected: OK");
}

// ═══════════════════════════════════════════════════════════════════════════
// ContractUpdate — Coordinator Signature Tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_contract_update_accepted_with_coordinator_signature() {
    let dag = make_dag();
    let policy = make_policy(Some(COORDINATOR_PK));

    let block = make_block(
        vec!["genesis".into()],
        PlainPayload::ContractUpdate {
            contract_id: "test-contract-001".into(),
            enabled: false,
            reason: "Maintenance".into(),
        },
        Some(COORDINATOR_PK),
    );

    let result = validate_block(&dag, &block, &policy);
    println!("ContractUpdate with coordinator sig: {:?}", result);
    assert!(result.is_ok());
    println!("ContractUpdate accepted with coordinator signature: OK");
}

#[test]
fn test_contract_update_rejected_without_coordinator_signature() {
    let dag = make_dag();
    let policy = make_policy(Some(COORDINATOR_PK));

    let block = make_block(
        vec!["genesis".into()],
        PlainPayload::ContractUpdate {
            contract_id: "test-contract-001".into(),
            enabled: false,
            reason: "Hacking attempt".into(),
        },
        Some(RANDOM_PK),
    );

    let result = validate_block(&dag, &block, &policy);
    println!("ContractUpdate with wrong sig: {:?}", result);
    assert!(result.is_err());
    println!("ContractUpdate rejected without coordinator sig: OK");
}

#[test]
fn test_contract_update_empty_contract_id_rejected() {
    let dag = make_dag();
    let policy = make_policy(Some(COORDINATOR_PK));

    let block = make_block(
        vec!["genesis".into()],
        PlainPayload::ContractUpdate {
            contract_id: "".into(),
            enabled: false,
            reason: "test".into(),
        },
        Some(COORDINATOR_PK),
    );

    let result = validate_block(&dag, &block, &policy);
    println!("ContractUpdate empty contract_id: {:?}", result);
    assert!(result.is_err());
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(err_msg.contains("contract_id"), "Error should mention contract_id");
    println!("ContractUpdate empty contract_id rejected: OK");
}

// ═══════════════════════════════════════════════════════════════════════════
// Scope Variations
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_contract_register_with_ledger_scope_accepted() {
    let dag = make_dag();
    let policy = make_policy(Some(COORDINATOR_PK));

    let contract = Contract {
        contract_id: "ledger-scoped".into(),
        name: "main-only-burn".into(),
        scope: ContractScope::Ledger(vec!["main".into(), "nft".into()]),
        trigger: ContractTrigger::OnNftBurn { nft_type: None },
        actions: vec![ContractAction::EmitEvent {
            event_type: "burn-notification".into(),
        }],
        enabled: true,
        version: 1,
    };

    let block = make_block(
        vec!["genesis".into()],
        PlainPayload::ContractRegister(contract),
        Some(COORDINATOR_PK),
    );

    let result = validate_block(&dag, &block, &policy);
    println!("Ledger-scoped ContractRegister: {:?}", result);
    assert!(result.is_ok());
    println!("Ledger-scoped contract accepted: OK");
}

#[test]
fn test_contract_register_token_burn_trigger_accepted() {
    let dag = make_dag();
    let policy = make_policy(Some(COORDINATOR_PK));

    let contract = Contract {
        contract_id: "token-burn-contract".into(),
        name: "edenite-burn".into(),
        scope: ContractScope::Global,
        trigger: ContractTrigger::OnTokenBurn {
            asset_id: "edenite".into(),
        },
        actions: vec![ContractAction::AccumulateRefund {
            asset_id: Some("pms".into()),
            formula: MintFormula::FixedAmount {
                amount: "100".into(),
            },
        }],
        enabled: true,
        version: 1,
    };

    let block = make_block(
        vec!["genesis".into()],
        PlainPayload::ContractRegister(contract),
        Some(COORDINATOR_PK),
    );

    let result = validate_block(&dag, &block, &policy);
    println!("TokenBurn trigger ContractRegister: {:?}", result);
    assert!(result.is_ok());
    println!("TokenBurn trigger contract accepted: OK");
}
