use anyhow::Result;
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use reqwest::StatusCode;
use std::time::{Duration, Instant};

pub async fn sync_after_submit(
    store: &RocksStore,
    id_opt: Option<&str>,
    status: StatusCode,
) -> Result<()> {
    // 201 = créé et persisté
    if status == StatusCode::CREATED {
        let _ = store.refresh_from_primary();
        return Ok(());
    }

    if status == StatusCode::ACCEPTED {
        if let Some(id) = id_opt {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let _ = store.refresh_from_primary();
                if let Ok(Some(_)) = store.get_block(&id.to_string()).await {
                    break;
                }
                if Instant::now() >= deadline {
                    eprintln!("ℹ️  Bloc pas encore visible après 2s (écriture async)");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        return Ok(());
    }

    if status == StatusCode::CONFLICT {
        let _ = store.refresh_from_primary();
    }
    Ok(())
}
