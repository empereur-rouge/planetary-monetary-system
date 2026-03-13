//! Tests d'intégration pour le ContractStorage (RocksDB).
//!
//! Vérifie le CRUD et le filtrage des contrats sur une DB éphémère.

use anyhow::Result;
use std::sync::Arc;
use tempfile::tempdir;

use pms_storage::rocks_store::store::RocksStore;
use pms_storage::ContractStorage;
use pms_types_contract::*;

async fn mk_store(prefix: &str) -> Result<(tempfile::TempDir, Arc<RocksStore>)> {
    let dir = tempdir()?;
    let path = dir.path().join(format!("rocks-contract-{}", nanoid::nanoid!(6)));
    std::fs::create_dir_all(&path)?;
    let path_str = path.to_string_lossy().to_string();
    let store = Arc::new(RocksStore::new(&path_str, 64, prefix, None).await?);
    // Run migrations to create all CFs including "contracts"
    store.ensure_schema().await?;
    Ok((dir, store))
}

fn make_contract(id: &str, name: &str, scope: ContractScope, nft_type: Option<&str>) -> Contract {
    Contract {
        contract_id: id.into(),
        name: name.into(),
        scope,
        trigger: ContractTrigger::OnNftBurn {
            nft_type: nft_type.map(String::from),
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

// ═══════════════════════════════════════════════════════════════════════════
// CRUD Tests
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_put_and_get_contract() -> Result<()> {
    let (_dir, store) = mk_store(&format!("ct:pg:{}", nanoid::nanoid!(6))).await?;

    let c = make_contract("c1", "cube-burn", ContractScope::Global, Some("cube"));
    store.put_contract(&c)?;

    let got = store.get_contract("c1")?;
    println!("get_contract('c1'): {:?}", got);
    assert!(got.is_some());

    let got = got.unwrap();
    assert_eq!(got.contract_id, "c1");
    assert_eq!(got.name, "cube-burn");
    assert!(got.enabled);
    assert_eq!(got.version, 1);
    println!("put_and_get: OK — contract_id={}, name={}", got.contract_id, got.name);
    Ok(())
}

#[tokio::test]
async fn test_get_nonexistent_returns_none() -> Result<()> {
    let (_dir, store) = mk_store(&format!("ct:ne:{}", nanoid::nanoid!(6))).await?;

    let got = store.get_contract("nonexistent")?;
    println!("get_contract('nonexistent'): {:?}", got);
    assert!(got.is_none());
    println!("nonexistent returns None: OK");
    Ok(())
}

#[tokio::test]
async fn test_list_contracts() -> Result<()> {
    let (_dir, store) = mk_store(&format!("ct:ls:{}", nanoid::nanoid!(6))).await?;

    // Empty initially
    let list = store.list_contracts()?;
    println!("list_contracts (empty): {:?}", list);
    assert!(list.is_empty());

    // Add 3 contracts
    store.put_contract(&make_contract("c1", "cube-burn", ContractScope::Global, Some("cube")))?;
    store.put_contract(&make_contract("c2", "ticket-rebate", ContractScope::Global, Some("ticket")))?;
    store.put_contract(&make_contract(
        "c3",
        "main-only",
        ContractScope::Ledger(vec!["main".into()]),
        None,
    ))?;

    let list = store.list_contracts()?;
    println!("list_contracts (3): {} contracts", list.len());
    for c in &list {
        println!("  - {} ({})", c.name, c.contract_id);
    }
    assert_eq!(list.len(), 3);
    println!("list_contracts: OK");
    Ok(())
}

#[tokio::test]
async fn test_put_overwrites_existing() -> Result<()> {
    let (_dir, store) = mk_store(&format!("ct:ow:{}", nanoid::nanoid!(6))).await?;

    let c1 = make_contract("c1", "cube-burn-v1", ContractScope::Global, Some("cube"));
    store.put_contract(&c1)?;

    // Overwrite with new version
    let mut c1v2 = c1.clone();
    c1v2.name = "cube-burn-v2".into();
    c1v2.version = 2;
    store.put_contract(&c1v2)?;

    let got = store.get_contract("c1")?.unwrap();
    println!("After overwrite: name={}, version={}", got.name, got.version);
    assert_eq!(got.name, "cube-burn-v2");
    assert_eq!(got.version, 2);
    println!("put_overwrites: OK");
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Toggle (set_enabled) Tests
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_set_enabled_toggle() -> Result<()> {
    let (_dir, store) = mk_store(&format!("ct:tog:{}", nanoid::nanoid!(6))).await?;

    let c = make_contract("c1", "cube-burn", ContractScope::Global, Some("cube"));
    store.put_contract(&c)?;

    // Initially enabled
    let got = store.get_contract("c1")?.unwrap();
    assert!(got.enabled);
    println!("Before toggle: enabled={}", got.enabled);

    // Disable
    store.set_enabled("c1", false)?;
    let got = store.get_contract("c1")?.unwrap();
    assert!(!got.enabled);
    println!("After disable: enabled={}", got.enabled);

    // Re-enable
    store.set_enabled("c1", true)?;
    let got = store.get_contract("c1")?.unwrap();
    assert!(got.enabled);
    println!("After re-enable: enabled={}", got.enabled);
    println!("set_enabled toggle: OK");
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// find_nft_burn_contracts Tests
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_find_by_nft_type() -> Result<()> {
    let (_dir, store) = mk_store(&format!("ct:fnt:{}", nanoid::nanoid!(6))).await?;

    store.put_contract(&make_contract("c1", "cube-burn", ContractScope::Global, Some("cube")))?;
    store.put_contract(&make_contract("c2", "ticket-rebate", ContractScope::Global, Some("ticket")))?;

    // Search for cube type
    let results = store.find_nft_burn_contracts(Some("cube"), "main")?;
    println!("find(cube): {} contracts", results.len());
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].contract_id, "c1");

    // Search for ticket type
    let results = store.find_nft_burn_contracts(Some("ticket"), "main")?;
    println!("find(ticket): {} contracts", results.len());
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].contract_id, "c2");

    // Search for unknown type
    let results = store.find_nft_burn_contracts(Some("unknown"), "main")?;
    println!("find(unknown): {} contracts", results.len());
    assert!(results.is_empty());

    println!("find_by_nft_type: OK");
    Ok(())
}

#[tokio::test]
async fn test_find_respects_scope() -> Result<()> {
    let (_dir, store) = mk_store(&format!("ct:fsc:{}", nanoid::nanoid!(6))).await?;

    // Global contract
    store.put_contract(&make_contract("c1", "global", ContractScope::Global, Some("cube")))?;
    // Local to "main" only
    store.put_contract(&make_contract(
        "c2",
        "main-only",
        ContractScope::Ledger(vec!["main".into()]),
        Some("cube"),
    ))?;
    // Local to "nft" only
    store.put_contract(&make_contract(
        "c3",
        "nft-only",
        ContractScope::Ledger(vec!["nft".into()]),
        Some("cube"),
    ))?;

    // On "main" ledger: should see global + main-only
    let results = store.find_nft_burn_contracts(Some("cube"), "main")?;
    println!("find(cube, main): {} contracts", results.len());
    for c in &results {
        println!("  - {} ({:?})", c.name, c.scope);
    }
    assert_eq!(results.len(), 2);

    // On "nft" ledger: should see global + nft-only
    let results = store.find_nft_burn_contracts(Some("cube"), "nft")?;
    println!("find(cube, nft): {} contracts", results.len());
    assert_eq!(results.len(), 2);

    // On "other" ledger: should see global only
    let results = store.find_nft_burn_contracts(Some("cube"), "other")?;
    println!("find(cube, other): {} contracts", results.len());
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "global");

    println!("find_respects_scope: OK");
    Ok(())
}

#[tokio::test]
async fn test_find_ignores_disabled() -> Result<()> {
    let (_dir, store) = mk_store(&format!("ct:fid:{}", nanoid::nanoid!(6))).await?;

    store.put_contract(&make_contract("c1", "cube-burn", ContractScope::Global, Some("cube")))?;
    store.set_enabled("c1", false)?;

    let results = store.find_nft_burn_contracts(Some("cube"), "main")?;
    println!("find(disabled): {} contracts", results.len());
    assert!(results.is_empty());
    println!("find_ignores_disabled: OK");
    Ok(())
}

#[tokio::test]
async fn test_find_wildcard_trigger() -> Result<()> {
    let (_dir, store) = mk_store(&format!("ct:fwt:{}", nanoid::nanoid!(6))).await?;

    // Wildcard contract (nft_type = None)
    store.put_contract(&make_contract("c1", "any-burn-rebate", ContractScope::Global, None))?;

    // Should match any nft_type
    let r1 = store.find_nft_burn_contracts(Some("cube"), "main")?;
    let r2 = store.find_nft_burn_contracts(Some("ticket"), "main")?;
    let r3 = store.find_nft_burn_contracts(None, "main")?;

    println!("wildcard(cube): {}, wildcard(ticket): {}, wildcard(None): {}",
        r1.len(), r2.len(), r3.len());
    assert_eq!(r1.len(), 1);
    assert_eq!(r2.len(), 1);
    assert_eq!(r3.len(), 1);
    println!("find_wildcard_trigger: OK");
    Ok(())
}
