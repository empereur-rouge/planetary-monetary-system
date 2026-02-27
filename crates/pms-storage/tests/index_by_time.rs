use anyhow::Result;
use pms_core::MAX_TIPS_CAP;
use pms_storage::{DagStorage, StoredBlock};
use pms_testkit::test_rocks_store_with_limit;
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
async fn index_by_time_grows_unbounded() -> Result<()> {
    // by_time is no longer trimmed (activity API needs full history).
    // Verify that ALL inserted blocks are retained.
    let tip_limit = MAX_TIPS_CAP;
    let store = test_rocks_store_with_limit("ix-time", tip_limit).await?;

    let total = tip_limit + 3;
    let mut inserted = Vec::with_capacity(total);
    for i in 0..total {
        let id = format!("blk-{}", i);
        let s = sb(&id);
        let _ = store.append_block_atomic(&s).await?;
        inserted.push(id);
        sleep(Duration::from_millis(2)).await;
    }

    // All blocks should be in by_time (no trimming)
    let recent = store.recent_ids(total + 10).await?;
    assert_eq!(
        recent.len(),
        total,
        "by_time should contain ALL {} entries (no trimming)",
        total
    );

    // Order: newest -> oldest
    let mut expected_desc: Vec<String> = inserted.clone();
    expected_desc.reverse();
    assert_eq!(
        recent, expected_desc,
        "recent_ids() must return all ids in newest->oldest order"
    );

    Ok(())
}
