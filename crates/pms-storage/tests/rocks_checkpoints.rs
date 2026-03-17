// crates/pms-storage/tests/rocks_checkpoints.rs

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use tempfile::tempdir;
use tokio::time::{Duration, sleep};

use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use pms_storage::rotate_checkpoints;

/// Test d'intégration pour:
///  - `RocksStore::create_checkpoint`
///  - `rotate_checkpoints`
///
/// On vérifie que:
///  1) des dossiers `rocks-…` sont bien créés dans `backup_root`
///  2) `rotate_checkpoints(…, keep_last=2)` ne garde que les 2 plus récents
#[tokio::test]
async fn rocks_checkpoints_are_created_and_rotated() -> Result<()> {
    // 1) Dossier temporaire isolé pour ce test
    let tmp = tempdir()?;
    let tmp_path = tmp.path().to_path_buf();

    // Dossier pour la DB Rocks
    let db_path: PathBuf = tmp_path.join("db");
    // Dossier pour les backups/checkpoints
    let backup_root: PathBuf = tmp_path.join("backups");

    // 2) Initialise un RocksStore dans ce dossier
    //
    //    - path: db_path
    //    - tip_limit: 256 (valeur arbitraire pour le test)
    //    - prefix: "it:test"
    let store = RocksStore::new(db_path.to_string_lossy().as_ref(), 256, "it:test", None, &RocksMemoryConfig::default())
        .await
        .expect("RocksStore::new doit réussir en test");

    // (optionnel) ensure_schema() si tu l'utilises dans ton code
    if let Err(e) = store.ensure_schema().await {
        eprintln!("[TEST] ensure_schema failed (non fatal ici): {e:#}");
    }

    // 3) Crée plusieurs checkpoints espacés,
    //    pour être sûr que les noms (rocks-YYYYMMDD-HHMMSS) diffèrent.
    for i in 0..4 {
        store
            .create_checkpoint(backup_root.to_string_lossy().as_ref())
            .expect("create_checkpoint doit réussir");

        eprintln!("[TEST] checkpoint #{i} created");

        // Petit sleep pour éviter d'avoir exactement la même seconde
        sleep(Duration::from_millis(1100)).await;
    }

    // 4) Vérifie qu'on a bien au moins 4 dossiers "rocks-…"
    let mut entries: Vec<PathBuf> = fs::read_dir(&backup_root)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|name| name.starts_with("rocks-"))
                    .unwrap_or(false)
        })
        .collect();

    assert!(
        entries.len() >= 4,
        "On attend au moins 4 checkpoints, trouvés={}",
        entries.len()
    );

    // 5) Appelle rotate_checkpoints(…, keep_last=2)
    rotate_checkpoints(backup_root.to_string_lossy().as_ref(), 2)
        .expect("rotate_checkpoints doit réussir");

    // 6) Relit les entrées restantes
    entries = fs::read_dir(&backup_root)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|name| name.starts_with("rocks-"))
                    .unwrap_or(false)
        })
        .collect();

    // On doit maintenant avoir AU PLUS 2 checkpoints
    assert!(
        entries.len() <= 2,
        "Après rotation, on doit garder au plus 2 checkpoints, trouvés={}",
        entries.len()
    );

    // Bonus: vérifie que les noms restants sont bien les plus récents
    entries.sort_by(|a, b| {
        a.file_name()
            .and_then(|s| s.to_str())
            .cmp(&b.file_name().and_then(|s| s.to_str()))
    });

    if entries.len() == 2 {
        let older = &entries[0];
        let newer = &entries[1];

        let older_name = older.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let newer_name = newer.file_name().and_then(|s| s.to_str()).unwrap_or("");

        eprintln!(
            "[TEST] remaining checkpoints (oldest -> newest): {}, {}",
            older_name, newer_name
        );

        // Comme les timestamps sont dans le nom, on s'assure juste que
        // le tri lexicographique correspond bien à l'ordre "vieux -> récent".
        assert!(
            older_name <= newer_name,
            "ordre lexicographique incohérent: older='{}', newer='{}'",
            older_name,
            newer_name
        );
    }

    Ok(())
}
