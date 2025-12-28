// crates/pms-storage/tests/rocks_integration.rs
use anyhow::Result;
use tempfile::{tempdir, TempDir};
use tokio::time::{sleep, Duration};
use std::sync::Arc;

use pms_storage::{DagStorage, StoredBlock};
use pms_storage::rocks_store::store::RocksStore;
use pms_types_block::BlockId;
use pms_types_payload::PlainPayload;
use pms_types_transaction::TxOutput;

// -- helper: crée une DB éphémère + RocksStore
struct TestStore {
    _dir: TempDir,                // garde la vie du dossier
    pub path: String,
    pub store: Arc<RocksStore>,
}

async fn mk_store(tip_limit: usize, prefix: &str) -> Result<TestStore> {
    let dir = tempdir()?;
    let path = dir.path().join(format!("rocks-{}", nanoid::nanoid!(6)));
    std::fs::create_dir_all(&path)?;
    let path_str = path.to_string_lossy().to_string();

    let store = Arc::new(RocksStore::new(&path_str, tip_limit, prefix).await?);
    Ok(TestStore { _dir: dir, path: path_str, store })
}

// utilité: petit bloc "valide" (Reward simple)
fn mk_block(id: &str, parents: Vec<BlockId>) -> StoredBlock {
    let payload = PlainPayload::Reward {
        outputs: vec![TxOutput { address: "addr".into(), amount: "1.0".into() }],
    };
    let payload_json = Some(serde_json::to_string(&payload).expect("serialize payload"));
    StoredBlock { id: id.to_string(), parents, payload_json, nonce: 1 }
}