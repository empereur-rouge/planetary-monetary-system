use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Supprime les checkpoints RocksDB les plus anciens dans `backup_root`
/// pour ne garder que `keep_last` snapshots au maximum.
///
/// Convention attendue :
///   backup_root/rocks-YYYYMMDD-HHMMSS
///
/// Exemple:
///   rotate_checkpoints("/var/backups/pms", 10)?;
pub fn rotate_checkpoints(backup_root: &str, keep_last: usize) -> Result<()> {
    // ------------------------------------------------------------
    // 0) Si keep_last == 0 → on ne garde rien (tout supprimer)
    // ------------------------------------------------------------
    if keep_last == 0 {
        eprintln!(
            "[rocks] rotate_checkpoints: keep_last=0 → tout supprimer dans {}",
            backup_root
        );
    }

    let root = PathBuf::from(backup_root);

    // Si le dossier n’existe pas → rien à faire.
    if !root.exists() {
        eprintln!(
            "[rocks] rotate_checkpoints: backup_root inexistant, skip: {}",
            root.display()
        );
        return Ok(());
    }

    // ------------------------------------------------------------
    // 1) Liste tous les enfants de backup_root
    // ------------------------------------------------------------
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(&root)
        .with_context(|| format!("read_dir({})", root.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        // On ne garde que les dossiers
        if !path.is_dir() {
            continue;
        }

        // On ne garde que ceux dont le nom commence par "rocks-"
        // (protection contre d'autres fichiers dans le même dossier)
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };

        if name.starts_with("rocks-") {
            entries.push(path);
        }
    }

    // Si aucun snapshot → rien à faire
    if entries.is_empty() {
        eprintln!(
            "[rocks] rotate_checkpoints: aucun checkpoint trouvé dans {}",
            root.display()
        );
        return Ok(());
    }

    // ------------------------------------------------------------
    // 2) Tri lexicographique par chemin (nom de dossier)
    // ------------------------------------------------------------
    // Grâce au format "rocks-YYYYMMDD-HHMMSS", le tri lexicographique
    // = tri chronologique (les plus anciens en premier).
    entries.sort_by(|a, b| {
        a.file_name()
            .and_then(|s| s.to_str())
            .cmp(&b.file_name().and_then(|s| s.to_str()))
    });

    let total = entries.len();
    eprintln!(
        "[rocks] rotate_checkpoints: trouvés {} checkpoints dans {}",
        total,
        root.display()
    );

    // ------------------------------------------------------------
    // 3) Si total <= keep_last → rien à supprimer
    // ------------------------------------------------------------
    if total <= keep_last {
        eprintln!(
            "[rocks] rotate_checkpoints: total ({}) <= keep_last ({}), skip",
            total, keep_last
        );
        return Ok(());
    }

    // ------------------------------------------------------------
    // 4) Calcule combien on doit supprimer (les plus anciens)
    // ------------------------------------------------------------
    let to_remove = total - keep_last;
    let (old, _recent) = entries.split_at(to_remove);

    eprintln!(
        "[rocks] rotate_checkpoints: suppression des {} plus anciens (on gardera {})",
        to_remove, keep_last
    );

    // ------------------------------------------------------------
    // 5) Supprime chaque ancien snapshot récursivement (rm -rf)
    // ------------------------------------------------------------
    for path in old {
        eprintln!("[rocks] deleting old checkpoint: {}", path.display());
        fs::remove_dir_all(path)
            .with_context(|| format!("remove_dir_all({})", path.display()))?;
    }

    Ok(())
}