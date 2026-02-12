// crates/pms-storage/tests/token_registry_test.rs
//
// Tests pour le registre de tokens multi-asset (RocksDB column family "token_registry").

use anyhow::Result;
use pms_storage::rocks_store::store::RocksStore;
use pms_types_payload::TokenMetadata;
use std::sync::Arc;
use tempfile::{TempDir, tempdir};

struct TestStore {
    _dir: TempDir,
    pub store: Arc<RocksStore>,
}

async fn mk_store(prefix: &str) -> Result<TestStore> {
    let dir = tempdir()?;
    let path = dir
        .path()
        .join(format!("rocks-token-{}", nanoid::nanoid!(5)));
    std::fs::create_dir_all(&path)?;
    let path_str = path.to_string_lossy().to_string();
    let store = Arc::new(RocksStore::new(&path_str, 64, prefix, None).await?);
    Ok(TestStore { _dir: dir, store })
}

fn edenite_metadata() -> TokenMetadata {
    TokenMetadata {
        asset_id: "edenite".into(),
        symbol: "EDEN".into(),
        name: "Edenite Token".into(),
        decimals: 8,
        max_supply: Some("1000000.00000000".into()),
        creator: "coordinator_pk_hex".into(),
        mint_authority: "coordinator_pk_hex".into(),
    }
}

fn gold_metadata() -> TokenMetadata {
    TokenMetadata {
        asset_id: "gold".into(),
        symbol: "GOLD".into(),
        name: "Gold Token".into(),
        decimals: 4,
        max_supply: None, // unlimited
        creator: "coordinator_pk_hex".into(),
        mint_authority: "coordinator_pk_hex".into(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// register_token
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn register_token_happy_path() -> Result<()> {
    let ts = mk_store("reg:ok").await?;
    let meta = edenite_metadata();

    ts.store.register_token(&meta)?;

    let found = ts.store.get_token("edenite")?;
    assert!(found.is_some(), "token should exist after registration");

    let found = found.unwrap();
    assert_eq!(found.asset_id, "edenite");
    assert_eq!(found.symbol, "EDEN");
    assert_eq!(found.name, "Edenite Token");
    assert_eq!(found.decimals, 8);
    assert_eq!(found.max_supply, Some("1000000.00000000".into()));
    assert_eq!(found.creator, "coordinator_pk_hex");
    assert_eq!(found.mint_authority, "coordinator_pk_hex");

    Ok(())
}

#[tokio::test]
async fn register_token_duplicate_fails() -> Result<()> {
    let ts = mk_store("reg:dup").await?;
    let meta = edenite_metadata();

    ts.store.register_token(&meta)?;

    // Tenter d'enregistrer le même token doit échouer
    let result = ts.store.register_token(&meta);
    assert!(result.is_err(), "duplicate registration should fail");
    assert!(
        result.unwrap_err().to_string().contains("already exists"),
        "error should mention 'already exists'"
    );

    Ok(())
}

#[tokio::test]
async fn register_multiple_tokens() -> Result<()> {
    let ts = mk_store("reg:multi").await?;

    ts.store.register_token(&edenite_metadata())?;
    ts.store.register_token(&gold_metadata())?;

    let eden = ts.store.get_token("edenite")?;
    let gold = ts.store.get_token("gold")?;

    assert!(eden.is_some());
    assert!(gold.is_some());
    assert_eq!(eden.unwrap().symbol, "EDEN");
    assert_eq!(gold.unwrap().symbol, "GOLD");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// get_token
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn get_token_not_found() -> Result<()> {
    let ts = mk_store("get:nf").await?;

    let result = ts.store.get_token("nonexistent")?;
    assert!(result.is_none(), "unknown token should return None");

    Ok(())
}

#[tokio::test]
async fn get_token_returns_correct_metadata() -> Result<()> {
    let ts = mk_store("get:ok").await?;

    ts.store.register_token(&gold_metadata())?;

    let gold = ts.store.get_token("gold")?.unwrap();
    assert_eq!(gold.decimals, 4);
    assert_eq!(gold.max_supply, None);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// list_tokens
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn list_tokens_empty() -> Result<()> {
    let ts = mk_store("list:e").await?;

    let tokens = ts.store.list_tokens()?;
    assert!(tokens.is_empty(), "empty registry should return empty list");

    Ok(())
}

#[tokio::test]
async fn list_tokens_returns_all() -> Result<()> {
    let ts = mk_store("list:all").await?;

    ts.store.register_token(&edenite_metadata())?;
    ts.store.register_token(&gold_metadata())?;

    let tokens = ts.store.list_tokens()?;
    assert_eq!(tokens.len(), 2);

    let ids: Vec<&str> = tokens.iter().map(|t| t.asset_id.as_str()).collect();
    assert!(ids.contains(&"edenite"));
    assert!(ids.contains(&"gold"));

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Sérialisation / Rétrocompatibilité
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn token_metadata_serialization_roundtrip() -> Result<()> {
    let meta = edenite_metadata();
    let json = serde_json::to_string(&meta)?;
    let back: TokenMetadata = serde_json::from_str(&json)?;
    assert_eq!(meta, back);

    Ok(())
}

#[tokio::test]
async fn token_metadata_without_max_supply() -> Result<()> {
    let meta = gold_metadata();
    let json = serde_json::to_string(&meta)?;

    // max_supply should be skipped when None
    assert!(
        !json.contains("max_supply"),
        "None max_supply should be skipped in JSON"
    );

    let back: TokenMetadata = serde_json::from_str(&json)?;
    assert_eq!(back.max_supply, None);

    Ok(())
}
