// tests/rocks_migration.rs

use anyhow::Result;
use pms_storage::rocks_store::store::RocksStore;
use std::path::PathBuf;

// ==== helpers ==== //

fn temp_db_path() -> PathBuf {
    // dossier unique genre target/tmp-rocks-<rand>
    let rand = nanoid::nanoid!();
    let mut p = std::env::temp_dir();
    p.push(format!("pms_rocks_test_{rand}"));
    p
}

#[tokio::test]
async fn migrations_apply_and_version_is_current() -> Result<()> {
    use pms_storage::migrations::CURRENT_VER;

    let path = temp_db_path();
    let prefix = format!("pms:test:{}", nanoid::nanoid!());
    let store = RocksStore::new(path.to_str().unwrap(), 64, prefix.clone(), None).await?;

    // si tu as une fonction équivalente à ensure_schema()
    store.ensure_schema().await?;

    // et si tu as une méthode style store.get_version() -> Result<i64>
    let v = store.get_version().await?;
    assert_eq!(v, CURRENT_VER);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════
// DAG Version Tests
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_dag_version_default() -> Result<()> {
    let path = temp_db_path();
    let prefix = format!("pms:test:{}", nanoid::nanoid!());
    let store = RocksStore::new(path.to_str().unwrap(), 64, prefix, None).await?;

    let version = store.get_dag_version().await?;
    println!("Default DAG version (fresh DB): '{}'", version);
    assert_eq!(version, "1.0.0", "Fresh DB should default to 1.0.0");

    Ok(())
}

#[tokio::test]
async fn test_dag_version_roundtrip() -> Result<()> {
    let path = temp_db_path();
    let prefix = format!("pms:test:{}", nanoid::nanoid!());
    let store = RocksStore::new(path.to_str().unwrap(), 64, prefix, None).await?;

    store.set_dag_version("1.2.3").await?;
    let version = store.get_dag_version().await?;
    println!("Set 1.2.3 -> got '{}'", version);
    assert_eq!(version, "1.2.3");

    store.set_dag_version("2.0.0").await?;
    let version2 = store.get_dag_version().await?;
    println!("Set 2.0.0 -> got '{}'", version2);
    assert_eq!(version2, "2.0.0");

    Ok(())
}

#[tokio::test]
async fn test_dag_version_persistence() -> Result<()> {
    let path = temp_db_path();
    let prefix = format!("pms:test:{}", nanoid::nanoid!());

    // Ouverture 1 : écrire la version
    {
        let store = RocksStore::new(path.to_str().unwrap(), 64, prefix.clone(), None).await?;
        store.set_dag_version("3.1.4").await?;
        let v = store.get_dag_version().await?;
        println!("Before close: '{}'", v);
        assert_eq!(v, "3.1.4");
    }

    // Ouverture 2 : vérifier que la version persiste
    {
        let store = RocksStore::new(path.to_str().unwrap(), 64, prefix, None).await?;
        let v = store.get_dag_version().await?;
        println!("After reopen: '{}'", v);
        assert_eq!(v, "3.1.4", "DAG version should survive DB reopen");
    }

    Ok(())
}
