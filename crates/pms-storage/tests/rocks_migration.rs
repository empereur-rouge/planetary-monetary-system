// tests/rocks_migration.rs

use anyhow::Result;
use std::path::PathBuf;
use pms_storage::rocks_store::store::RocksStore;

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
    let store = RocksStore::new(path.to_str().unwrap(), 64, prefix.clone()).await?;

    // si tu as une fonction équivalente à ensure_schema()
    store.ensure_schema().await?;

    // et si tu as une méthode style store.get_version() -> Result<i64>
    let v = store.get_version().await?;
    assert_eq!(v, CURRENT_VER);

    Ok(())
}