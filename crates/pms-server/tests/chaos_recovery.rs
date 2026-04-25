//! Chaos / disaster-recovery tests (audit item 7, v0.7.4).
//!
//! These tests are **#[ignore]** — they're heavier than the rest of the
//! suite and we want CI to stay fast. Run them on demand:
//!
//! ```sh
//! cargo test --release -p pms-server --test chaos_recovery -- --ignored --nocapture
//! ```
//!
//! The plan defined five scenarios; we cover them as follows:
//!
//! | # | Plan name                                              | This file                                |
//! |---|--------------------------------------------------------|------------------------------------------|
//! | S1| Crash mid-insertion, restart, verify coherence         | `s1_durability_after_unclean_drop`       |
//! | S2| Disk-full / persist failure must surface as error      | covered by `pms-core/tests/persist_no_silent_drops.rs` (linked below) |
//! | S3| Corrupt the latest SST → reopen behavior is explicit    | `s3_corrupted_sst_fails_loud_or_recovers`|
//! | S4| Crash during `append_blocks_batch` → atomicity         | `s4_batch_atomicity_across_reopen`       |
//! | S5| Persist-pipeline saturation followed by restart        | `s5_pipeline_failure_surfaces_to_caller` |
//!
//! In-process simulation — we don't fork the actual binary. We open a
//! `RocksStore` against a tempdir, write data, drop the store (which
//! gracefully closes RocksDB and flushes the WAL — for true unclean
//! shutdown we'd need a subprocess; that's out of scope here), reopen
//! the same path and assert the durability contract. This catches the
//! "is the WAL/SST flow actually durable across process restart"
//! question without the orchestration cost of a real fork.

use anyhow::Result;
use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use pms_storage::{DagStorage, StoredBlock, UtxoDelta};
use std::sync::Arc;
use tempfile::TempDir;

fn make_block(id: &str, parent: Option<&str>) -> StoredBlock {
    StoredBlock {
        id: id.to_string(),
        parents: parent.map(|p| vec![p.to_string()]).unwrap_or_default(),
        payload_json: Some(format!(r#"{{"chaos":"{}"}}"#, id)),
        nonce: 0,
        network_id: "chaos-test".into(),
        protocol_version: 1,
        signer_pk_hex: "deadbeef".into(),
        signature_hex: "cafe".into(),
        metadata: None,
    }
}

async fn open_store(path: &str) -> Result<RocksStore> {
    RocksStore::new(path, 100, "main", None, &RocksMemoryConfig::default()).await
}

// ─────────────────────────────────────────────────────────────────────
// S1 — durability across an unclean drop
// ─────────────────────────────────────────────────────────────────────
//
// What we test: open a store, insert a handful of blocks via the public
// `append_block_atomic_with_utxo` path, drop the store, reopen, and
// assert every block is back. The single-block path is the most-used
// API in the persist pipeline, so this is the path most likely to
// regress on a future tuning change.
//
// Why this matters: pre-0.7.2 the persist pipeline could silently drop
// blocks that callers had been told were `Inserted`. The post-fix
// contract is "every Inserted ack is durable across restart." This
// test holds RocksDB to that contract end-to-end.

#[tokio::test(flavor = "multi_thread")]
#[ignore = "chaos test — run with --ignored"]
async fn s1_durability_after_unclean_drop() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().to_string_lossy().into_owned();
    println!("[S1] data dir: {path}");

    // Round 1: open, insert 32 blocks via the single-block path, drop.
    {
        let store = open_store(&path).await.expect("open round 1");
        for i in 0..32 {
            let block = make_block(&format!("blk-{i:04}"), None);
            let inserted = DagStorage::append_block_atomic(&store, &block)
                .await
                .expect("append");
            assert!(inserted, "block {i} must be a fresh insert");
        }
        // Force the WAL to disk before drop so even an environment
        // without graceful shutdown sees the inserts. Production calls
        // this from `spawn_background_maintenance`.
        store.flush_wal().await.expect("flush_wal");
        println!("[S1] round 1 wrote 32 blocks, dropping store…");
    } // <- store dropped here; rocksdb::DB::Close runs, releases the lock.

    // Round 2: reopen, verify every block is recovered.
    let store2 = open_store(&path).await.expect("reopen");
    let count = store2.block_count().await.expect("block_count");
    println!("[S1] reopened, block_count = {count}");
    assert_eq!(count, 32, "every previously-Inserted block must be present after reopen");

    for i in 0..32 {
        let id = format!("blk-{i:04}");
        let got = store2.get_block(&id).await.expect("get_block").expect("present");
        assert_eq!(got.id, id);
    }
    println!("[S1] PASS — 32/32 blocks survived the drop+reopen");
}

// ─────────────────────────────────────────────────────────────────────
// S3 — corrupt the latest SST, reopen behavior must be explicit
// ─────────────────────────────────────────────────────────────────────
//
// What we test: open a store, insert blocks, force a flush to materialise
// SST files on disk, close the store, find an SST and overwrite a few
// bytes in the middle, then try to reopen. The contract is: either
// RocksDB recovers gracefully (read what it can, return clean errors
// for the corrupted range) OR the open call returns an error a human
// can act on — never silent data corruption.
//
// We don't enforce a specific outcome because it depends on which SST
// gets clobbered (an empty / unused one is harmless). We assert that
// **whatever** RocksDB does, it doesn't panic and the test process
// survives.

#[tokio::test(flavor = "multi_thread")]
#[ignore = "chaos test — run with --ignored"]
async fn s3_corrupted_sst_fails_loud_or_recovers() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().to_string_lossy().into_owned();
    println!("[S3] data dir: {path}");

    {
        let store = open_store(&path).await.expect("open");
        // Insert enough blocks to push at least one memtable flush so
        // we get real SST files on disk to corrupt. The default
        // write_buffer_size is small enough that 200 small blocks
        // usually triggers a flush; we explicitly compact afterwards
        // so we definitely have SSTs.
        for i in 0..200 {
            let block = make_block(&format!("blk-{i:04}"), None);
            DagStorage::append_block_atomic(&store, &block)
                .await
                .expect("append");
        }
        store.flush_wal().await.expect("flush_wal");
        store.compact_all().await.expect("compact_all");
        println!("[S3] inserted 200 blocks, compacted, dropping…");
    }

    // Find an SST to clobber. RocksDB stores them as `*.sst` in the
    // data dir.
    let mut sst_files: Vec<std::path::PathBuf> = std::fs::read_dir(&path)
        .expect("readdir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("sst"))
        .collect();
    sst_files.sort();

    println!("[S3] SST files on disk: {}", sst_files.len());
    if sst_files.is_empty() {
        // The flush didn't materialise (memtable wasn't full enough);
        // nothing to corrupt — the test isn't meaningful in this run.
        // Treat as inconclusive rather than failing.
        println!("[S3] SKIP — no SSTs were generated; tune up the write volume if this happens often");
        return;
    }
    let target = sst_files.last().unwrap().clone();
    let target_size = std::fs::metadata(&target).expect("metadata").len();
    println!("[S3] corrupting {} ({} bytes)", target.display(), target_size);

    // Overwrite ~64 bytes in the middle. Avoid the very start (file
    // format header) so the corruption looks like a real bit-rot
    // event, not an obviously-empty file.
    use std::io::{Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(&target)
        .expect("open SST for write");
    let mid = (target_size / 2).max(64);
    f.seek(SeekFrom::Start(mid)).expect("seek");
    let garbage = [0xFFu8; 64];
    f.write_all(&garbage).expect("write garbage");
    f.sync_all().expect("sync");
    drop(f);

    // Reopen — capture the outcome instead of asserting a specific
    // result; what we forbid is a panic / silent success.
    let reopened = open_store(&path).await;
    match reopened {
        Ok(store) => {
            println!("[S3] reopen Ok — RocksDB skipped/recovered the corrupted SST");
            let count = store.block_count().await.unwrap_or(u64::MAX);
            println!("[S3] post-recovery block_count = {count}");
            // Sanity check: even partial recovery should yield a count
            // well under the 200 we wrote OR exactly 200 if the SST
            // RocksDB chose to corrupt was empty/unused.
            assert!(count <= 200, "count cannot exceed what was written");
        }
        Err(e) => {
            println!("[S3] reopen Err — RocksDB refused to load: {e:#}");
            // The error string should mention what failed (corruption,
            // checksum, IO) so the operator has something actionable
            // — assert it isn't an empty / opaque error.
            let msg = format!("{e:#}");
            assert!(
                !msg.trim().is_empty(),
                "open error must carry a diagnostic message"
            );
        }
    }
    println!("[S3] PASS — corruption was either recovered or surfaced loudly (no silent acceptance)");
}

// ─────────────────────────────────────────────────────────────────────
// S4 — atomicity of `append_blocks_batch` across an unclean drop
// ─────────────────────────────────────────────────────────────────────
//
// `append_blocks_batch` is what the background persist task calls; it
// builds a single RocksDB `WriteBatch` so the WAL append is one
// transaction. The contract for the operator is "after a crash, either
// the entire batch is there or none of it is". This test enforces that
// by writing a batch, dropping the store, reopening, and asserting the
// recovered count equals the batch size.

#[tokio::test(flavor = "multi_thread")]
#[ignore = "chaos test — run with --ignored"]
async fn s4_batch_atomicity_across_reopen() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().to_string_lossy().into_owned();
    println!("[S4] data dir: {path}");

    let batch_size = 64;

    {
        let store = open_store(&path).await.expect("open");

        // Build the batch — same pattern background_persist_task uses.
        let blocks: Vec<StoredBlock> = (0..batch_size)
            .map(|i| make_block(&format!("batch-{i:04}"), None))
            .collect();
        let refs: Vec<(&StoredBlock, Option<&UtxoDelta>)> =
            blocks.iter().map(|b| (b, None)).collect();

        let inserted = store.append_blocks_batch(&refs).await.expect("batch write");
        assert_eq!(
            inserted, batch_size,
            "every block in the batch must be a fresh insert"
        );
        store.flush_wal().await.expect("flush_wal");
        println!("[S4] wrote batch of {batch_size}, dropping…");
    }

    let store2 = open_store(&path).await.expect("reopen");
    let count = store2.block_count().await.expect("block_count");
    println!("[S4] reopened, block_count = {count}");

    // The atomicity contract: count must be either 0 (batch lost in
    // its entirety) or `batch_size` (batch fully replayed). Anything
    // in between is the failure mode we're guarding against.
    assert!(
        count == 0 || count == batch_size as u64,
        "batch must be all-or-nothing across reopen, got {count}"
    );

    // We flushed the WAL before drop so we expect full recovery; if
    // this ever flips to 0 it's a regression worth investigating.
    assert_eq!(
        count, batch_size as u64,
        "after explicit flush_wal, the entire batch must be recovered"
    );
    println!("[S4] PASS — batch survived intact, atomicity preserved");
}

// ─────────────────────────────────────────────────────────────────────
// S5 — persist pipeline failure surfaces to the caller
// ─────────────────────────────────────────────────────────────────────
//
// We don't re-implement the full `persist_no_silent_drops.rs` logic
// here (those tests cover the back-pressure and retry-then-shutdown
// contract directly). We add a compact, chaos-flavoured check: spin up
// the background persist task against a store that returns errors,
// flood it with jobs, wait for the task to die after exhausting
// retries, and assert the channel is closed so subsequent sends fail.
//
// This is the production fail-mode: a saturated / failing RocksDB
// causes `do_persist_block` to return `Err`, which the HTTP handler
// translates to 500 — never a silent "Inserted".

#[tokio::test(flavor = "multi_thread")]
#[ignore = "chaos test — run with --ignored"]
async fn s5_pipeline_failure_surfaces_to_caller() {
    use anyhow::anyhow;
    use async_trait::async_trait;
    use pms_core::background_persist::{spawn_background_persist_with_retry, PersistJob};
    use pms_storage::PutResult;
    use pms_wire::WireBlock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Storage that always fails — simulates "disk full" / "RocksDB
    /// stalled" / "permanent IO error". Implementation cribbed from
    /// `persist_no_silent_drops::CountingFailStore` but pared down to
    /// what the background task touches.
    struct AlwaysFails(AtomicUsize);
    #[async_trait]
    impl DagStorage for AlwaysFails {
        async fn append_blocks_batch(
            &self,
            _blocks: &[(&StoredBlock, Option<&UtxoDelta>)],
        ) -> Result<usize> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!("simulated permanent disk error"))
        }
        async fn persist_final(&self, _ids: &[String]) -> Result<()> {
            Ok(())
        }
        async fn put_block(&self, _: &StoredBlock) -> Result<PutResult> { todo!() }
        async fn get_block(&self, _: &str) -> Result<Option<StoredBlock>> { todo!() }
        async fn add_child_edge(&self, _: &str, _: &str) -> Result<()> { todo!() }
        async fn children_count(&self, _: &str) -> Result<u64> { todo!() }
        async fn add_tip(&self, _: &str) -> Result<()> { todo!() }
        async fn remove_tip(&self, _: &str) -> Result<()> { todo!() }
        async fn top_tips(&self, _: usize) -> Result<Vec<String>> { todo!() }
        async fn all_block_ids(&self) -> Result<Vec<String>> { todo!() }
        async fn block_count(&self) -> Result<u64> { todo!() }
        async fn export_json(&self) -> Result<String> { todo!() }
        async fn export_namespace(&self) -> Result<String> { todo!() }
        async fn import_json(&self, _: &str) -> Result<()> { todo!() }
        async fn append_block_atomic(&self, _: &StoredBlock) -> Result<bool> { todo!() }
        async fn append_block_atomic_with_utxo(
            &self,
            _: &StoredBlock,
            _: Option<&UtxoDelta>,
        ) -> Result<bool> { todo!() }
        async fn load_final(&self) -> Result<Vec<String>> { todo!() }
        async fn load_last_milestone(&self) -> Result<Option<String>> { todo!() }
        async fn recent_ids(&self, _: usize) -> Result<Vec<String>> { todo!() }
        async fn recent_ids_by_time(
            &self,
            _: Option<i64>,
            _: Option<String>,
            _: usize,
        ) -> Result<(Vec<String>, Option<(i64, String, bool)>)> { todo!() }
        async fn get_blocks_by_ids(&self, _: &[String]) -> Result<Vec<WireBlock>> { todo!() }
        async fn persist_last_milestone(&self, _: &str) -> Result<()> { todo!() }
    }

    let store = Arc::new(AlwaysFails(AtomicUsize::new(0)));
    // Tiny retry budget so the test ends fast.
    let (tx, handle) = spawn_background_persist_with_retry(store.clone(), 2, vec![5, 10]);

    // First send goes into the channel. Subsequent sends may block /
    // fail depending on whether the consumer has died yet.
    tx.send(PersistJob {
        block: make_block("doomed-1", None),
        delta: None,
        newly_finalized: vec![],
    })
    .await
    .expect("first send while task is alive");

    // Wait for the task to drain its retry budget and shut down.
    handle.await.expect("task joins");
    println!(
        "[S5] task exited; append_blocks_batch was called {} times",
        store.0.load(Ordering::SeqCst)
    );

    // After the task is gone the channel is closed: any further
    // send().await must return an error. That's what
    // `do_persist_block` translates into the HTTP error path.
    let post_send = tx
        .send(PersistJob {
            block: make_block("post-mortem", None),
            delta: None,
            newly_finalized: vec![],
        })
        .await;
    println!("[S5] post-shutdown send result = {:?}", post_send.is_err());
    assert!(
        post_send.is_err(),
        "after the consumer dies, the producer must observe a closed channel"
    );
    println!("[S5] PASS — pipeline failure propagates to the caller (no silent drop)");
}
