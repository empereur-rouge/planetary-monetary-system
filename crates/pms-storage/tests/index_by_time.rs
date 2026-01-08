use anyhow::Result;
use pms_core::MAX_TIPS_CAP;
use pms_storage::{DagStorage, StoredBlock};
use pms_testkit::{test_rocks_store, test_rocks_store_with_limit};
use tokio::time::{Duration, sleep};

fn sb(id: &str) -> StoredBlock {
    StoredBlock {
        id: id.into(),
        parents: vec![],
        payload_json: None,
        nonce: 0,
        network_id: "".to_string(),
        protocol_version: 0,
        signer_pk_hex: "".to_string(),
        signature_hex: "".to_string(),
        metadata: None,
    }
}

#[tokio::test]
async fn index_by_time_respects_tip_limit_rocks() -> Result<()> {
    // Paramètres
    let tip_limit = MAX_TIPS_CAP;
    // DB éphémère
    let store = test_rocks_store_with_limit("ix-time", tip_limit).await?;

    // Insert plus de blocs que tip_limit (chaque append met à jour l’index temps + trim)
    let total = tip_limit + 3;
    let mut inserted = Vec::with_capacity(total);
    for i in 0..total {
        let id = format!("blk-{}", i);
        let s = sb(&id);
        // append atomique => index by_time + trim auto
        let _ = store.append_block_atomic(&s).await?;
        inserted.push(id);
        // assure des timestamps distincts
        sleep(Duration::from_millis(2)).await;
    }

    // Récupère les ids récents (ordre: du plus récent au plus ancien)
    let recent = store.recent_ids(tip_limit + 10).await?;
    assert_eq!(
        recent.len(),
        tip_limit,
        "by_time devrait contenir exactement tip_limit entrées"
    );

    // Les tip_limit derniers insérés DOIVENT être là (en ordre inverse: newest -> oldest)
    let keep_slice = &inserted[inserted.len() - tip_limit..]; // [oldest_kept .. newest]
    let mut keep_desc: Vec<String> = keep_slice.iter().cloned().collect();
    keep_desc.reverse(); // newest -> oldest

    assert_eq!(
        recent, keep_desc,
        "recent_ids() doit retourner exactement les {} derniers ids (newest->oldest)",
        tip_limit
    );

    // Les premiers (expulsés) NE doivent PAS apparaître dans recent_ids()
    for evicted in &inserted[..inserted.len() - tip_limit] {
        assert!(
            !recent.contains(evicted),
            "id {} devrait être expulsé de l'index temporel",
            evicted
        );
    }

    Ok(())
}
