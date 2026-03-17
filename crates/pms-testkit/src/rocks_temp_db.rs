use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use std::fs;
use std::sync::Arc;
use std::time::Duration;
use tempfile::{TempDir, tempdir};
use tokio::net::TcpStream;
use tokio::time::{Instant, sleep};

/// Crée un RocksStore éphémère pour tests, avec un prefix unique.
///
/// - `suffix` permet d’identifier le scénario de test (ex: "final", "kdepth").
/// - La DB est stockée dans un dossier temporaire qui sera supprimé à la fin du test.
pub struct TestRocksStore {
    pub store: Arc<RocksStore>,
    _dir: TempDir, // juste pour la durée de vie
}

pub async fn test_rocks_store(name: &str) -> anyhow::Result<TestRocksStore> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(format!("rocks-{name}"));
    std::fs::create_dir_all(&path)?;
    let path_str = path.to_string_lossy().to_string();

    let store = Arc::new(RocksStore::new(&path_str, 128, format!("it:{name}"), None, &RocksMemoryConfig::default()).await?);

    Ok(TestRocksStore { store, _dir: dir })
}

pub async fn test_rocks_store_with_limit(
    name: &str,
    tip_limit: usize,
) -> anyhow::Result<Arc<RocksStore>> {
    let dir = tempdir()?;
    let db = dir.path().join(name);
    fs::create_dir_all(&db)?;
    let db_path = db.to_string_lossy().to_string();

    // prefix libre pour les tests
    let prefix = format!("it:{}", name);

    let store = RocksStore::new(&db_path, tip_limit, &prefix, None, &RocksMemoryConfig::default()).await?;
    // si tu as ensure_schema async:
    store.ensure_schema().await?;
    Ok(Arc::new(store))
}

/// Génère un prefix unique (utile si tu ajoutes d’autres artefacts nommés).
pub fn unique_prefix(suffix: &str) -> String {
    format!("pms:test:{}:{}", suffix, nanoid::nanoid!())
}

pub async fn wait_for_listen(addr: &str, max_wait_ms: u64) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_millis(max_wait_ms);
    loop {
        match TcpStream::connect(addr).await {
            Ok(_) => return Ok(()), // listener prêt
            Err(_) if Instant::now() < deadline => sleep(Duration::from_millis(20)).await,
            Err(e) => return Err(e.into()),
        }
    }
}
